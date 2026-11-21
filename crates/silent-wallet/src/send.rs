//! Building and signing a transaction.
//!
//! **This is the only module that uses the spend private key**, and the one
//! to read first in a review. It writes no cryptography of its own. Every
//! operation on keys is a call into the libraries this workspace pins:
//!
//! * which output keys pay a silent payment address:
//!   `bdk_sp::send::bip352::SpSender::send_to` (upstream);
//! * the private key of one of our outputs: `SecretKey::add_tweak`
//!   (`secp256k1`), spend key plus the output's tweak — the same call
//!   upstream's own signer makes;
//! * the signature: `SighashCache::taproot_key_spend_signature_hash`
//!   (`bitcoin`) and `Secp256k1::sign_schnorr_no_aux_rand` (`secp256k1`).
//!
//! What this module adds is bookkeeping and checks, and it refuses to hand
//! out a transaction unless all of them hold:
//!
//! * the key derived for an input is the key of that input's output script;
//! * every signature verifies against that key;
//! * the fee is what the inputs leave after the outputs, and within limits;
//! * the wallet's own scanner, given the finished transaction, finds the
//!   change output with the expected amount — so change is never sent to a
//!   key the wallet could not find again.
//!
//! Tweaks are derived when the transaction is built
//! ([`silent_sync::spend::recover_spendable`]) and dropped when this
//! function returns. Nothing here stores, prints or returns a key or a
//! tweak.

use crate::keys::Wallet;
use bdk_sp::{
    bitcoin::{
        absolute::LockTime,
        key::{Keypair, TweakedPublicKey},
        secp256k1::{Message, Scalar, Secp256k1},
        sighash::{Prevouts, SighashCache},
        taproot,
        transaction::Version,
        Address, Amount, OutPoint, ScriptBuf, Sequence, TapSighashType, Transaction, TxIn, TxOut,
        Txid, Witness,
    },
    encoding::SilentPaymentCode,
    receive::SpOut,
    send::bip352::SpSender,
};
use serde::Serialize;
use silent_sync::{
    protocol::TxScanOutcome,
    spend::{recover_spendable, SpendError, SpendableOutput},
    DetectedOutput, Network, ScanSource, SilentPaymentAddress,
};
use std::str::FromStr;

/// The change label BIP352 reserves.
const CHANGE_LABEL: u32 = 0;

/// Where a payment goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Destination {
    /// A silent payment address.
    SilentPayment(SilentPaymentAddress),
    /// An ordinary address, as its output script, with its text form.
    Address {
        /// The address as given.
        text: String,
        /// The script that pays it.
        script_pubkey: ScriptBuf,
    },
}

impl Destination {
    /// Reads an address for `network`: a silent payment address or an
    /// ordinary one. An address for another network — mainnet included — is
    /// refused.
    pub fn parse(text: &str, network: Network) -> Result<Self, SendError> {
        let text = text.trim();
        let looks_silent = ["sp1", "tsp1", "sprt1"]
            .iter()
            .any(|prefix| text.to_ascii_lowercase().starts_with(prefix));
        if looks_silent {
            return SilentPaymentAddress::parse(text, network)
                .map(Destination::SilentPayment)
                .map_err(|e| SendError::Destination(e.to_string()));
        }
        let address = Address::from_str(text)
            .map_err(|e| SendError::Destination(format!("not an address: {e}")))?
            .require_network(network.to_bitcoin())
            .map_err(|_| SendError::Destination(format!("not a {network} address")))?;
        Ok(Destination::Address {
            text: text.to_string(),
            script_pubkey: address.script_pubkey(),
        })
    }

    fn describe(&self) -> String {
        match self {
            Destination::SilentPayment(address) => address.to_string(),
            Destination::Address { text, .. } => text.clone(),
        }
    }
}

/// One payment to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Payment {
    /// Where to.
    pub to: Destination,
    /// How much, in satoshi.
    pub amount_sats: u64,
}

/// Limits a transaction must stay within; exceeding one is an error, never a
/// silent adjustment.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SendLimits {
    /// Highest fee rate accepted, in satoshi per virtual byte.
    pub max_fee_rate: u64,
    /// Highest total fee accepted, in satoshi.
    pub max_fee_sats: u64,
    /// Most inputs one transaction may spend.
    pub max_inputs: usize,
}

impl Default for SendLimits {
    fn default() -> Self {
        Self {
            max_fee_rate: 500,
            max_fee_sats: 100_000,
            max_inputs: 200,
        }
    }
}

impl SendLimits {
    /// Highest total fee accepted, in satoshi.
    pub fn with_max_fee_sats(mut self, max_fee_sats: u64) -> Self {
        self.max_fee_sats = max_fee_sats;
        self
    }
}

/// Why no transaction was produced.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    #[error("{0}")]
    /// The destination could not be read, or is for another network.
    Destination(String),
    #[error("nothing to pay")]
    /// No payment was given.
    NoPayments,
    #[error(
        "{amount_sats} sat to {to} is below the smallest amount the network relays ({dust} sat)"
    )]
    /// A payment is too small to be relayed.
    BelowDust {
        /// Where to.
        to: String,
        /// The amount asked for.
        amount_sats: u64,
        /// The smallest relayed amount for that kind of output.
        dust: u64,
    },
    #[error("the fee rate must be between 1 and {max} sat/vB, got {given}")]
    /// The fee rate is zero or above the limit.
    FeeRate {
        /// The rate asked for.
        given: u64,
        /// The limit.
        max: u64,
    },
    #[error(
        "not enough unspent outputs: {available} sat in {outputs} output(s), but the payments and \
         the fee need at least {needed} sat"
    )]
    /// The candidates do not cover the payments and the fee.
    InsufficientFunds {
        /// Sum of the candidate outputs.
        available: u64,
        /// Number of candidate outputs.
        outputs: usize,
        /// What the payments and the fee need at least.
        needed: u64,
    },
    #[error("the payment needs more than {max} inputs")]
    /// More inputs would be needed than the limit allows.
    TooManyInputs {
        /// The limit.
        max: usize,
    },
    #[error("the fee would be {fee_sats} sat, above the limit of {max} sat")]
    /// The fee exceeds the limit.
    FeeTooHigh {
        /// The fee the transaction would pay.
        fee_sats: u64,
        /// The limit.
        max: u64,
    },
    #[error(transparent)]
    /// An input could not be prepared for spending.
    Input(#[from] SpendError),
    #[error("the key derived for input {outpoint} is not the key of that output")]
    /// Spend key plus tweak did not give the output's key: these keys cannot
    /// spend it.
    KeyMismatch {
        /// The input.
        outpoint: OutPoint,
    },
    #[error("the upstream sender failed: {0}")]
    /// The upstream library could not derive the output keys.
    Upstream(String),
    #[error("internal check failed: {0}; no transaction was produced")]
    /// A check on the finished transaction failed.
    Check(&'static str),
}

/// An input of a built transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpentInput {
    /// The output it spends.
    pub outpoint: OutPoint,
    /// Its value in satoshi.
    pub value_sats: u64,
}

/// A payment output of a built transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaidOutput {
    /// Index of the output.
    pub vout: u32,
    /// Where it pays, as given.
    pub to: String,
    /// Its value in satoshi.
    pub amount_sats: u64,
}

/// The change output of a built transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChangeOutput {
    /// Index of the output.
    pub vout: u32,
    /// Its value in satoshi.
    pub amount_sats: u64,
}

/// A signed transaction and what it does. Contains no key material: a
/// signed transaction is public once it is sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct BuiltTransaction {
    /// The signed transaction.
    pub transaction: Transaction,
    /// Its id.
    pub txid: Txid,
    /// What it spends.
    pub inputs: Vec<SpentInput>,
    /// What it pays, in the order the payments were given.
    pub payments: Vec<PaidOutput>,
    /// The change back to this wallet (label 0), if any.
    pub change: Option<ChangeOutput>,
    /// The fee: inputs minus outputs.
    pub fee_sats: u64,
    /// Virtual size in bytes.
    pub vsize: u64,
}

/// A taproot output script of the usual size, for size estimates.
fn placeholder_p2tr() -> ScriptBuf {
    let mut bytes = vec![0x51, 0x20];
    bytes.extend_from_slice(&[0x02; 32]);
    ScriptBuf::from_bytes(bytes)
}

fn unsigned(inputs: &[OutPoint], outputs: Vec<TxOut>) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs
            .iter()
            .map(|outpoint| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: outputs,
    }
}

/// Virtual size of a transaction with `inputs` key-path taproot inputs and
/// the given output scripts, once signed.
fn signed_vsize(inputs: usize, output_scripts: &[ScriptBuf]) -> u64 {
    let mut tx = unsigned(
        &vec![OutPoint::null(); inputs],
        output_scripts
            .iter()
            .map(|script_pubkey| TxOut {
                value: Amount::ZERO,
                script_pubkey: script_pubkey.clone(),
            })
            .collect(),
    );
    for input in &mut tx.input {
        input.witness = Witness::from_slice(&[[0u8; 64]]);
    }
    tx.vsize() as u64
}

/// Selects inputs from `candidates` in the order given, builds a transaction
/// that makes `payments` and returns the rest to this wallet's change
/// address, and signs it.
///
/// * `candidates` must be outputs of this wallet that are unspent as far as
///   the caller knows, detected through full blocks. Each one that is used
///   is fetched again from `source` and scanned again before it is signed
///   for.
/// * `fee_rate` is in satoshi per virtual byte.
///
/// Nothing is sent: the caller decides what to do with the result.
pub fn build_and_sign(
    wallet: &Wallet,
    source: &dyn ScanSource,
    candidates: &[DetectedOutput],
    payments: &[Payment],
    fee_rate: u64,
    limits: &SendLimits,
) -> Result<BuiltTransaction, SendError> {
    let (keys, network, scanner) = (wallet.keys(), wallet.network(), wallet.scanner());
    let receiver = wallet.receiver();
    if payments.is_empty() {
        return Err(SendError::NoPayments);
    }
    if fee_rate == 0 || fee_rate > limits.max_fee_rate {
        return Err(SendError::FeeRate {
            given: fee_rate,
            max: limits.max_fee_rate,
        });
    }

    // Sizes do not depend on which taproot key an output pays, so the
    // amounts can be settled before any key is derived.
    let size_scripts: Vec<ScriptBuf> = payments
        .iter()
        .map(|p| match &p.to {
            Destination::SilentPayment(_) => placeholder_p2tr(),
            Destination::Address { script_pubkey, .. } => script_pubkey.clone(),
        })
        .collect();
    for (payment, script) in payments.iter().zip(&size_scripts) {
        let dust = script.minimal_non_dust().to_sat();
        if payment.amount_sats < dust {
            return Err(SendError::BelowDust {
                to: payment.to.describe(),
                amount_sats: payment.amount_sats,
                dust,
            });
        }
    }
    let total: u64 = payments
        .iter()
        .try_fold(0u64, |sum, p| sum.checked_add(p.amount_sats))
        .ok_or(SendError::Check("the payments overflow"))?;
    let mut with_change = size_scripts.clone();
    with_change.push(placeholder_p2tr());
    let fee_for = |inputs: usize, scripts: &[ScriptBuf]| signed_vsize(inputs, scripts) * fee_rate;

    // Take candidates in the order given until payments and fee are covered.
    let mut selected: Vec<&DetectedOutput> = Vec::new();
    let mut gathered = 0u64;
    let mut needed = total;
    for candidate in candidates {
        if selected.len() == limits.max_inputs {
            return Err(SendError::TooManyInputs {
                max: limits.max_inputs,
            });
        }
        selected.push(candidate);
        gathered = gathered
            .checked_add(candidate.value_sats)
            .ok_or(SendError::Check("the inputs overflow"))?;
        needed = total + fee_for(selected.len(), &size_scripts);
        if gathered >= needed {
            break;
        }
    }
    if selected.is_empty() || gathered < needed {
        return Err(SendError::InsufficientFunds {
            available: candidates.iter().map(|c| c.value_sats).sum(),
            outputs: candidates.len(),
            needed: needed.max(total + fee_for(selected.len().max(1), &size_scripts)),
        });
    }
    // Change, if what is left after the fee of a transaction *with* a change
    // output is worth an output; otherwise the remainder is fee.
    let after_fee_with_change = gathered.checked_sub(total + fee_for(selected.len(), &with_change));
    let change_sats = after_fee_with_change
        .filter(|left| *left >= placeholder_p2tr().minimal_non_dust().to_sat());
    let fee_sats = gathered - total - change_sats.unwrap_or(0);
    if fee_sats > limits.max_fee_sats {
        return Err(SendError::FeeTooHigh {
            fee_sats,
            max: limits.max_fee_sats,
        });
    }

    // Everything below needs the inputs' tweaks. They are derived here, from
    // the blocks themselves, and dropped when this function returns.
    let spendable: Vec<SpendableOutput> = selected
        .iter()
        .map(|d| recover_spendable(source, scanner, d))
        .collect::<Result<_, _>>()?;
    let secp = Secp256k1::new();
    let spend_secret = keys.spend_secret();
    let mut keypairs = Vec::with_capacity(spendable.len());
    for input in &spendable {
        let output_secret = spend_secret
            .add_tweak(&Scalar::from(input.sensitive_tweak()))
            .map_err(|_| SendError::KeyMismatch {
                outpoint: input.outpoint,
            })?;
        let keypair = Keypair::from_secret_key(&secp, &output_secret);
        let (output_key, _) = keypair.x_only_public_key();
        let script = &input.txout.script_pubkey;
        if !script.is_p2tr() || script.as_bytes()[2..] != output_key.serialize() {
            return Err(SendError::KeyMismatch {
                outpoint: input.outpoint,
            });
        }
        keypairs.push((keypair, output_key));
    }

    // The output keys of the silent payments — the recipients' and our
    // change — from the upstream sender.
    let code = |address: &SilentPaymentAddress| {
        SilentPaymentCode::new_v0(
            address.scan_public_key(),
            address.spend_public_key(),
            network.to_bitcoin(),
        )
    };
    let change_address = receiver
        .labelled_address(network, CHANGE_LABEL)
        .map_err(|e| SendError::Upstream(e.to_string()))?;
    let mut codes: Vec<SilentPaymentCode> = payments
        .iter()
        .filter_map(|p| match &p.to {
            Destination::SilentPayment(address) => Some(code(address)),
            Destination::Address { .. } => None,
        })
        .collect();
    if change_sats.is_some() {
        codes.push(code(&change_address));
    }
    let mut derived = if codes.is_empty() {
        Default::default()
    } else {
        let as_upstream: Vec<SpOut> = spendable
            .iter()
            .map(|input| SpOut {
                outpoint: input.outpoint,
                tweak: input.sensitive_tweak(),
                script_pubkey: input.txout.script_pubkey.clone(),
                amount: input.txout.value,
                label: input.label,
            })
            .collect();
        SpSender::new(spend_secret)
            .send_to(&as_upstream, &codes)
            .map_err(|e| SendError::Upstream(e.to_string()))?
    };
    let mut next_script = |address: &SilentPaymentAddress| -> Result<ScriptBuf, SendError> {
        let keys = derived
            .get_mut(&code(address))
            .filter(|keys| !keys.is_empty())
            .ok_or(SendError::Check("the sender returned no key for an output"))?;
        Ok(ScriptBuf::new_p2tr_tweaked(
            TweakedPublicKey::dangerous_assume_tweaked(keys.remove(0)),
        ))
    };

    let mut outputs = Vec::with_capacity(payments.len() + 1);
    let mut paid = Vec::with_capacity(payments.len());
    for payment in payments {
        let script_pubkey = match &payment.to {
            Destination::SilentPayment(address) => next_script(address)?,
            Destination::Address { script_pubkey, .. } => script_pubkey.clone(),
        };
        paid.push(PaidOutput {
            vout: outputs.len() as u32,
            to: payment.to.describe(),
            amount_sats: payment.amount_sats,
        });
        outputs.push(TxOut {
            value: Amount::from_sat(payment.amount_sats),
            script_pubkey,
        });
    }
    let change = match change_sats {
        Some(amount_sats) => {
            let vout = outputs.len() as u32;
            outputs.push(TxOut {
                value: Amount::from_sat(amount_sats),
                script_pubkey: next_script(&change_address)?,
            });
            Some(ChangeOutput { vout, amount_sats })
        }
        None => None,
    };

    // Sign every input with the key of its output, and verify the result.
    let outpoints: Vec<OutPoint> = spendable.iter().map(|input| input.outpoint).collect();
    let prevouts: Vec<TxOut> = spendable.iter().map(|input| input.txout.clone()).collect();
    let mut transaction = unsigned(&outpoints, outputs);
    let mut witnesses = Vec::with_capacity(keypairs.len());
    {
        let mut cache = SighashCache::new(&transaction);
        for (index, (keypair, output_key)) in keypairs.iter().enumerate() {
            let sighash = cache
                .taproot_key_spend_signature_hash(
                    index,
                    &Prevouts::All(&prevouts),
                    TapSighashType::Default,
                )
                .map_err(|_| SendError::Check("the signature hash could not be computed"))?;
            let message = Message::from(sighash);
            let signature = secp.sign_schnorr_no_aux_rand(&message, keypair);
            secp.verify_schnorr(&signature, &message, output_key)
                .map_err(|_| SendError::Check("a signature does not verify"))?;
            witnesses.push(Witness::p2tr_key_spend(&taproot::Signature {
                signature,
                sighash_type: TapSighashType::Default,
            }));
        }
    }
    for (input, witness) in transaction.input.iter_mut().zip(witnesses) {
        input.witness = witness;
    }

    // What leaves this function must be what was decided above.
    let spent: u64 = prevouts.iter().map(|o| o.value.to_sat()).sum();
    let sent: u64 = transaction.output.iter().map(|o| o.value.to_sat()).sum();
    if spent != gathered || spent.checked_sub(sent) != Some(fee_sats) {
        return Err(SendError::Check("the fee is not what was computed"));
    }
    let vsize = transaction.vsize() as u64;
    if fee_sats < vsize * fee_rate {
        return Err(SendError::Check("the fee is below the rate asked for"));
    }
    // The wallet must be able to find its own change: scan the finished
    // transaction the way a received one is scanned.
    if let Some(change) = &change {
        let found = match scanner.scan_tx(&transaction, &prevouts) {
            Ok(TxScanOutcome::Scanned { matches }) => matches,
            _ => Vec::new(),
        };
        let txid = transaction.compute_txid();
        let detected = found.iter().any(|m| {
            m.outpoint == OutPoint::new(txid, change.vout)
                && m.amount.to_sat() == change.amount_sats
                && m.label == Some(CHANGE_LABEL)
        });
        if !detected {
            return Err(SendError::Check(
                "the wallet's own scanner does not find the change output",
            ));
        }
    }

    Ok(BuiltTransaction {
        txid: transaction.compute_txid(),
        inputs: spendable
            .iter()
            .map(|input| SpentInput {
                outpoint: input.outpoint,
                value_sats: input.txout.value.to_sat(),
            })
            .collect(),
        payments: paid,
        change,
        fee_sats,
        vsize,
        transaction,
    })
}
