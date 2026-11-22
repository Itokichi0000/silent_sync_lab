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
//! * when there is change: the wallet's own scanner, given the finished
//!   transaction, finds the change output with the expected amount — so
//!   change is never sent to a key the wallet could not find again.
//!
//! What is **not** checked independently: that the key computed for someone
//! else's silent payment address is the one that recipient will find (only
//! their scan key could tell). That rests on the upstream sender, on this
//! project's tests of it against its own scanner, and on a real node and a
//! second wallet in the end-to-end test.
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
    /// Highest fee accepted as a percentage of the total paid.
    pub max_fee_percent: u64,
}

impl Default for SendLimits {
    fn default() -> Self {
        Self {
            max_fee_rate: 500,
            max_fee_sats: 100_000,
            max_inputs: 200,
            max_fee_percent: 50,
        }
    }
}

impl SendLimits {
    /// Highest total fee accepted, in satoshi.
    pub fn with_max_fee_sats(mut self, max_fee_sats: u64) -> Self {
        self.max_fee_sats = max_fee_sats;
        self
    }

    /// Highest fee accepted as a percentage of the total paid.
    pub fn with_max_fee_percent(mut self, max_fee_percent: u64) -> Self {
        self.max_fee_percent = max_fee_percent;
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
    #[error(
        "this wallet is on {wallet}, but {what} is on {found}; nothing is signed across networks"
    )]
    /// The source, or an output offered as an input, is on another network
    /// than the wallet.
    AnotherNetwork {
        /// The wallet's network.
        wallet: Network,
        /// What is elsewhere: the data source, or an output.
        what: String,
        /// Where it is.
        found: Network,
    },
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
    #[error(
        "the fee would be {fee_sats} sat for a payment of {paid_sats} sat: more than \
         {max_percent}% of what is paid"
    )]
    /// The fee is large next to the payment.
    FeeTooHighForAmount {
        /// The fee the transaction would pay.
        fee_sats: u64,
        /// The total of the payments.
        paid_sats: u64,
        /// The limit, as a percentage of the total paid.
        max_percent: u64,
    },
    #[error("an amount exceeds what can exist ({max} sat)")]
    /// A payment, the payments together or the inputs exceed the total
    /// supply: no real transaction has such amounts.
    AmountTooLarge {
        /// The largest amount that can exist.
        max: u64,
    },
    #[error("the output {outpoint} was offered as an input more than once")]
    /// The same output appears twice among the candidates.
    DuplicateInput {
        /// The output.
        outpoint: OutPoint,
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
    #[error("the data source could not say where the chain is: {0}")]
    /// The source's tip is needed for the transaction's lock time.
    Tip(String),
    #[error("the operating system gave no random numbers; no transaction was produced")]
    /// Input and output order and the lock time are drawn at random; without
    /// a source of randomness nothing is built.
    NoRandomness,
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
    /// What it pays, in the order the payments were given (their places in
    /// the transaction are random: see `vout`).
    pub payments: Vec<PaidOutput>,
    /// The change back to this wallet (label 0), if any.
    pub change: Option<ChangeOutput>,
    /// The fee: inputs minus outputs. It can be more than size × rate by
    /// less than one output's worth: what is too small to return as change.
    pub fee_sats: u64,
    /// The fee rate that was asked for, in satoshi per virtual byte.
    pub fee_rate_asked: u64,
    /// Whether the inputs were received under more than one label (or some
    /// under a label and some under none). Spending them together shows an
    /// observer that they belong to one wallet. `false` whenever one group
    /// alone could pay.
    pub mixes_labels: bool,
    /// Virtual size in bytes.
    pub vsize: u64,
}

/// A taproot output script of the usual size, for size estimates.
fn placeholder_p2tr() -> ScriptBuf {
    let mut bytes = vec![0x51, 0x20];
    bytes.extend_from_slice(&[0x02; 32]);
    ScriptBuf::from_bytes(bytes)
}

/// A uniformly random number below `bound`, from the operating system.
/// Used for ordering and the lock time only — never for keys or signatures.
fn random_below(bound: u64) -> Result<u64, SendError> {
    if bound == 0 {
        return Err(SendError::Check("a random number below zero was asked for"));
    }
    // Numbers at or above the largest multiple of `bound` are drawn again,
    // so that no result is more likely than another.
    let limit = u64::MAX - (u64::MAX % bound);
    loop {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).map_err(|_| SendError::NoRandomness)?;
        let number = u64::from_le_bytes(bytes);
        if number < limit {
            return Ok(number % bound);
        }
    }
}

/// Puts `items` in a uniformly random order.
fn shuffle<T>(items: &mut [T]) -> Result<(), SendError> {
    for last in (1..items.len()).rev() {
        let other = random_below(last as u64 + 1)? as usize;
        items.swap(last, other);
    }
    Ok(())
}

/// The lock time Bitcoin Core's wallet gives its transactions: the height
/// of the tip, so that the transaction cannot be mined into a block that
/// replaces the tip ("fee sniping") — and, one time in ten, up to 99 blocks
/// lower, as Core does for transactions that were signed some time before
/// they were sent. A transaction with lock time 0 stands out among them.
fn lock_time_like_core(tip_height: u32) -> Result<LockTime, SendError> {
    let mut height = tip_height;
    if random_below(10)? == 0 {
        height = height.saturating_sub(random_below(100)? as u32);
    }
    LockTime::from_height(height).map_err(|_| SendError::Check("the tip is not a block height"))
}

/// The largest candidates a search for a payment without change looks at,
/// and how many combinations it tries.
const EXACT_SEARCH_CANDIDATES: usize = 64;
const EXACT_SEARCH_TRIES: u32 = 100_000;

/// How a sum of inputs compares with what a payment without change needs.
enum Fit {
    TooLittle,
    Exact,
    TooMuch,
}

/// A search for inputs that pay without leaving change: depth first,
/// largest values first. `fit(count, sum)` judges a set; every input is
/// worth more than the fee for spending it, so a set that is too much stays
/// too much when another input is added.
struct ExactSearch<'a> {
    values: &'a [u64],
    max_inputs: usize,
    fit: &'a dyn Fn(usize, u64) -> Fit,
    tries: u32,
    picked: Vec<usize>,
}

impl ExactSearch<'_> {
    /// Tries the values from index `from` on, with `sum` picked so far and
    /// `left` still to choose from. On `true`, `picked` holds the set.
    fn run(&mut self, from: usize, sum: u64, left: u64) -> bool {
        if self.tries == 0 {
            return false;
        }
        self.tries -= 1;
        if !self.picked.is_empty() {
            match (self.fit)(self.picked.len(), sum) {
                Fit::Exact => return true,
                Fit::TooMuch => return false,
                Fit::TooLittle => {}
            }
        }
        if from == self.values.len()
            || self.picked.len() == self.max_inputs
            || matches!(
                (self.fit)(self.picked.len() + 1, sum.saturating_add(left)),
                Fit::TooLittle
            )
        {
            return false;
        }
        let value = self.values[from];
        let left = left.saturating_sub(value);
        self.picked.push(from);
        if self.run(from + 1, sum.saturating_add(value), left) {
            return true;
        }
        self.picked.pop();
        self.run(from + 1, sum, left)
    }
}

fn unsigned(inputs: &[OutPoint], outputs: Vec<TxOut>, lock_time: LockTime) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time,
        input: inputs
            .iter()
            .map(|outpoint| TxIn {
                previous_output: *outpoint,
                script_sig: ScriptBuf::new(),
                // Does not ask to be replaceable: this wallet has no way to
                // replace a transaction on purpose. This is a request, not
                // a protection: nodes that replace any transaction for a
                // higher fee (Bitcoin Core's default since 28) ignore it.
                // The value also lets the lock time take effect.
                sequence: Sequence::ENABLE_LOCKTIME_NO_RBF,
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
        LockTime::ZERO,
    );
    for input in &mut tx.input {
        input.witness = Witness::from_slice(&[[0u8; 64]]);
    }
    tx.vsize() as u64
}

/// Selects inputs from `candidates`, builds a transaction that makes
/// `payments` and returns the rest to this wallet's change address, and
/// signs it.
///
/// Which inputs: outputs received under different labels were paid by
/// different parties (or for different purposes), and spending them together
/// shows that they belong to one wallet. So each group — one label, or no
/// label — is tried on its own, in the order the groups first appear among
/// the candidates: first for a set that pays without change (nothing then
/// leads back to this wallet), then in the order given. Only if no group can
/// pay alone are all candidates used, in the order given, and the result
/// says so ([`BuiltTransaction::mixes_labels`]).
///
/// What an observer cannot tell from the result: inputs and outputs are in
/// random order (the change is not "the last output"), and the lock time is
/// the one Bitcoin Core's wallet would set.
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
    // A wallet signs for its own network only. Scripts and signatures name
    // no network, so nothing later would notice data from another chain.
    if source.chain().network() != network {
        return Err(SendError::AnotherNetwork {
            wallet: network,
            what: "the data source".to_string(),
            found: source.chain().network(),
        });
    }
    if let Some(foreign) = candidates.iter().find(|d| d.network != network) {
        return Err(SendError::AnotherNetwork {
            wallet: network,
            what: format!("the output {}:{}", foreign.txid, foreign.vout),
            found: foreign.network,
        });
    }
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
    // No amount here may exceed what can exist, and nothing below may wrap:
    // a sum that wrapped would pass every later comparison.
    let max_money = Amount::MAX_MONEY.to_sat();
    let too_large = || SendError::AmountTooLarge { max: max_money };
    let add = |a: u64, b: u64| a.checked_add(b).filter(|sum| *sum <= max_money);
    let mut total = 0u64;
    for payment in payments {
        total = add(total, payment.amount_sats).ok_or_else(too_large)?;
    }
    let mut with_change = size_scripts.clone();
    with_change.push(placeholder_p2tr());
    let fee_for = |inputs: usize, scripts: &[ScriptBuf]| {
        signed_vsize(inputs, scripts).saturating_mul(fee_rate)
    };

    // What can be spent at all: no output twice, and none that is worth no
    // more than the fee for spending it.
    let cost_of_adding = fee_for(2, &size_scripts).saturating_sub(fee_for(1, &size_scripts));
    let mut pool: Vec<&DetectedOutput> = Vec::new();
    let mut taken = std::collections::BTreeSet::new();
    let mut usable = (0u64, 0usize);
    for candidate in candidates {
        let outpoint = OutPoint::new(candidate.txid, candidate.vout);
        if !taken.insert(outpoint) {
            return Err(SendError::DuplicateInput { outpoint });
        }
        if candidate.value_sats <= cost_of_adding {
            continue;
        }
        usable = (
            add(usable.0, candidate.value_sats).ok_or_else(too_large)?,
            usable.1 + 1,
        );
        pool.push(candidate);
    }
    let fee_within_limits = |fee_sats: u64| {
        fee_sats <= limits.max_fee_sats
            && u128::from(fee_sats) * 100 <= u128::from(total) * u128::from(limits.max_fee_percent)
    };
    // Paying a little more fee instead of making change is worth it up to
    // what the change would cost: the output now, and spending it later.
    let cost_of_change = fee_for(1, &with_change)
        .saturating_sub(fee_for(1, &size_scripts))
        .saturating_add(cost_of_adding);
    // Inputs from `group` that pay without change, if the search finds any.
    let without_change = |group: &[&'_ DetectedOutput]| -> Option<Vec<usize>> {
        let mut largest: Vec<usize> = (0..group.len()).collect();
        largest.sort_by_key(|&i| std::cmp::Reverse(group[i].value_sats));
        largest.truncate(EXACT_SEARCH_CANDIDATES);
        let values: Vec<u64> = largest.iter().map(|&i| group[i].value_sats).collect();
        let fit = |count: usize, sum: u64| {
            let Some(needed) = add(total, fee_for(count, &size_scripts)) else {
                return Fit::TooLittle;
            };
            match sum.checked_sub(needed) {
                None => Fit::TooLittle,
                Some(extra)
                    if extra <= cost_of_change && fee_within_limits(sum.saturating_sub(total)) =>
                {
                    Fit::Exact
                }
                Some(_) => Fit::TooMuch,
            }
        };
        let mut search = ExactSearch {
            values: &values,
            max_inputs: limits.max_inputs,
            fit: &fit,
            tries: EXACT_SEARCH_TRIES,
            picked: Vec::new(),
        };
        let all = values.iter().fold(0u64, |a, v| a.saturating_add(*v));
        search
            .run(0, 0, all)
            .then(|| search.picked.iter().map(|&i| largest[i]).collect())
    };
    // Inputs from `group` in the order given, until payments and fee are
    // covered. `Ok(None)`: the group does not hold enough.
    let in_order = |group: &[&'_ DetectedOutput]| -> Result<Option<Vec<usize>>, SendError> {
        let mut selected = Vec::new();
        let mut gathered = 0u64;
        for (index, candidate) in group.iter().enumerate() {
            let needed =
                add(total, fee_for(selected.len().max(1), &size_scripts)).ok_or_else(too_large)?;
            if !selected.is_empty() && gathered >= needed {
                break;
            }
            if selected.len() == limits.max_inputs {
                return Err(SendError::TooManyInputs {
                    max: limits.max_inputs,
                });
            }
            selected.push(index);
            gathered = add(gathered, candidate.value_sats).ok_or_else(too_large)?;
        }
        let needed =
            add(total, fee_for(selected.len().max(1), &size_scripts)).ok_or_else(too_large)?;
        Ok((!selected.is_empty() && gathered >= needed).then_some(selected))
    };

    // One label (or none) at a time, in the order the groups first appear.
    let mut groups: Vec<(Option<u32>, Vec<&DetectedOutput>)> = Vec::new();
    for &candidate in &pool {
        match groups
            .iter_mut()
            .find(|(label, _)| *label == candidate.label)
        {
            Some((_, group)) => group.push(candidate),
            None => groups.push((candidate.label, vec![candidate])),
        }
    }
    fn pick<'a>(group: &[&'a DetectedOutput], picked: Vec<usize>) -> Vec<&'a DetectedOutput> {
        picked.into_iter().map(|i| group[i]).collect()
    }
    let mut choice: Option<(Vec<&DetectedOutput>, bool)> = None;
    for (_, group) in &groups {
        if let Some(picked) = without_change(group) {
            choice = Some((pick(group, picked), true));
            break;
        }
    }
    if choice.is_none() {
        for (_, group) in &groups {
            // A group that needs too many inputs is passed over like one
            // that holds too little.
            if let Ok(Some(picked)) = in_order(group) {
                choice = Some((pick(group, picked), false));
                break;
            }
        }
    }
    let (mut selected, exact) = match choice {
        Some(choice) => choice,
        // No group pays alone: everything, in the order given.
        None => match in_order(&pool)? {
            Some(picked) => (pick(&pool, picked), false),
            None => {
                return Err(SendError::InsufficientFunds {
                    available: usable.0,
                    outputs: usable.1,
                    needed: add(total, fee_for(usable.1.max(1), &size_scripts))
                        .ok_or_else(too_large)?,
                })
            }
        },
    };
    let mut gathered = 0u64;
    for input in &selected {
        gathered = add(gathered, input.value_sats).ok_or_else(too_large)?;
    }
    let mixes_labels = selected.iter().any(|d| d.label != selected[0].label);
    // The order of the inputs says nothing about the wallet.
    shuffle(&mut selected)?;
    // Change, if what is left after the fee of a transaction *with* a change
    // output is worth an output; otherwise the remainder is fee. Inputs that
    // were chosen to pay without change leave none.
    let dust = placeholder_p2tr().minimal_non_dust().to_sat();
    let change_sats = add(total, fee_for(selected.len(), &with_change))
        .and_then(|spent| gathered.checked_sub(spent))
        .filter(|left| *left >= dust)
        .filter(|_| !exact);
    let fee_sats = gathered
        .checked_sub(total)
        .and_then(|left| left.checked_sub(change_sats.unwrap_or(0)))
        .ok_or(SendError::Check("the fee is negative"))?;
    if fee_sats > limits.max_fee_sats {
        return Err(SendError::FeeTooHigh {
            fee_sats,
            max: limits.max_fee_sats,
        });
    }
    // A fee that is large next to what is paid is more likely a mistake (a
    // wrong rate, a tiny payment) than an intention.
    if u128::from(fee_sats) * 100 > u128::from(total) * u128::from(limits.max_fee_percent) {
        return Err(SendError::FeeTooHighForAmount {
            fee_sats,
            paid_sats: total,
            max_percent: limits.max_fee_percent,
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

    // Where an output stands says nothing about what it is: every output,
    // the change included, gets a random place.
    let mut place: Vec<usize> = (0..outputs.len()).collect();
    shuffle(&mut place)?;
    let mut placed = outputs.clone();
    for (was, output) in outputs.into_iter().enumerate() {
        placed[place[was]] = output;
    }
    let outputs = placed;
    for payment in &mut paid {
        payment.vout = place[payment.vout as usize] as u32;
    }
    let change = change.map(|change| ChangeOutput {
        vout: place[change.vout as usize] as u32,
        ..change
    });
    let tip = source.tip().map_err(|e| SendError::Tip(e.to_string()))?;
    let lock_time = lock_time_like_core(tip.height)?;

    // Sign every input with the key of its output, and verify the result.
    let outpoints: Vec<OutPoint> = spendable.iter().map(|input| input.outpoint).collect();
    let prevouts: Vec<TxOut> = spendable.iter().map(|input| input.txout.clone()).collect();
    let mut transaction = unsigned(&outpoints, outputs, lock_time);
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
        fee_rate_asked: fee_rate,
        mixes_labels,
        vsize,
        transaction,
    })
}
