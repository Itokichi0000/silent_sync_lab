//! BIP352 receive-side scanning, delegated to the upstream `bdk_sp` scanner.
//!
//! This module contains **no elliptic-curve arithmetic or hashing of its own**.
//! It adds, around the upstream [`Scanner`]:
//!
//! * the BIP352 "must be scanned if and only if" eligibility checks that only
//!   inspect scripts, including the "does not spend SegWit version > 1" rule
//!   that upstream does not implement (`docs/DECISIONS.md` D-006);
//! * a fail-closed check that every input has its previous output; upstream
//!   silently truncates on a length mismatch (D-007);
//! * BIP352's "stop at K_max" result where upstream returns an error (D-005);
//! * a classification of upstream errors into "transaction is not eligible"
//!   (normal, per BIP352) versus real errors;
//! * containment of upstream panics, because sources are not consensus
//!   validated by this crate and may hand over malformed data (D-008).

use crate::{
    bitcoin::{
        key::Secp256k1,
        secp256k1::{self, PublicKey, Scalar, SecretKey},
        Network, ScriptBuf, Transaction, TxOut,
    },
    network::LabNetwork,
};
use bdk_sp::{
    encoding::SilentPaymentCode,
    receive::{extract_pubkey, scan::Scanner, SpOut, SpReceiveError},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    panic::{catch_unwind, AssertUnwindSafe},
};

/// BIP352 per-group recipient limit, as defined by the upstream scanner.
pub use bdk_sp::receive::K_MAX;

/// BIP352 change label integer. It is always scanned for (BIP352 "Scanning").
pub const CHANGE_LABEL: u32 = 0;

/// Receive-side key material: scan private key and spend *public* key.
///
/// The spend private key is never needed for detection and is not accepted.
/// `Debug` is redacted and the type is neither `Serialize` nor `Display`, so
/// the scan key cannot leak through logs or exports by accident.
#[derive(Clone)]
pub struct ReceiverKeys {
    scan_sk: SecretKey,
    spend_pk: PublicKey,
}

impl ReceiverKeys {
    pub fn new(scan_sk: SecretKey, spend_pk: PublicKey) -> Self {
        Self { scan_sk, spend_pk }
    }

    pub fn scan_public_key(&self) -> PublicKey {
        self.scan_sk.public_key(&Secp256k1::new())
    }

    pub fn spend_public_key(&self) -> PublicKey {
        self.spend_pk
    }

    /// The unlabelled silent payment address for `network`.
    pub fn address(&self, network: LabNetwork) -> SilentPaymentCode {
        self.code(network.to_bitcoin())
    }

    /// The labelled silent payment address for label integer `m`.
    pub fn labelled_address(
        &self,
        network: LabNetwork,
        m: u32,
    ) -> Result<SilentPaymentCode, ProtocolError> {
        let label = SilentPaymentCode::get_label(self.scan_sk, m);
        self.address(network)
            .add_label(label)
            .map_err(|e| ProtocolError::Label(e.to_string()))
    }

    fn code(&self, network: Network) -> SilentPaymentCode {
        SilentPaymentCode::new_v0(self.scan_public_key(), self.spend_pk, network)
    }
}

impl fmt::Debug for ReceiverKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceiverKeys")
            .field("scan_sk", &"<redacted>")
            .field("spend_pk", &self.spend_pk)
            .finish()
    }
}

/// Why a transaction was not scanned. These are normal outcomes defined by
/// BIP352 ("Scanning silent payment eligible transactions"), not errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkipReason {
    /// Coinbase transactions have no inputs for shared secret derivation.
    Coinbase,
    /// No BIP341 taproot output.
    NoTaprootOutputs,
    /// No input from the "Inputs For Shared Secret Derivation" list.
    NoEligibleInputs,
    /// The transaction spends an output with SegWit version > 1.
    SpendsUnknownSegwitVersion,
    /// The eligible input public keys sum to the point at infinity.
    InputKeySumIsInfinity,
}

impl SkipReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            SkipReason::Coinbase => "coinbase",
            SkipReason::NoTaprootOutputs => "no_taproot_outputs",
            SkipReason::NoEligibleInputs => "no_eligible_inputs",
            SkipReason::SpendsUnknownSegwitVersion => "spends_unknown_segwit_version",
            SkipReason::InputKeySumIsInfinity => "input_key_sum_is_infinity",
        }
    }
}

/// Result of looking at one transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxScanOutcome {
    /// BIP352 says this transaction must not be scanned.
    NotEligible(SkipReason),
    /// The transaction was scanned. `matches` may be empty.
    ///
    /// The upstream [`SpOut`] carries the per-output tweak; callers that do not
    /// need it (everything in this lab) must drop it rather than store it.
    Scanned { matches: Vec<SpOut> },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("transaction has {inputs} inputs but {prevouts} previous outputs were supplied")]
    PrevoutCountMismatch { inputs: usize, prevouts: usize },
    #[error(
        "more than K_max={0} outputs matched even after capping the scan (docs/DECISIONS.md D-005)"
    )]
    KMaxExceeded(u32),
    #[error("K_max fallback found {0} matches instead of exactly K_max; refusing to guess")]
    KMaxFallbackInconsistent(usize),
    #[error("could not derive label: {0}")]
    Label(String),
    #[error("upstream scanner error: {0}")]
    Upstream(String),
    #[error("upstream scanner panicked on malformed input; transaction treated as an error")]
    UpstreamPanic,
}

/// Scanner for one receiver (one scan key / spend key pair plus labels).
pub struct ReceiveScanner {
    inner: Scanner,
    labels: BTreeSet<u32>,
}

impl ReceiveScanner {
    /// Scanner that checks for the unlabelled address, the change label
    /// (`m = 0`, always, as BIP352 requires) and every label in `labels`.
    pub fn new(keys: &ReceiverKeys, labels: &[u32]) -> Result<Self, ProtocolError> {
        let mut all: BTreeSet<u32> = labels.iter().copied().collect();
        all.insert(CHANGE_LABEL);
        Self::build(keys, all)
    }

    /// Scanner that checks for exactly `labels` and nothing else — in
    /// particular **not** for the change label unless it is listed.
    ///
    /// This exists for conformance testing against the BIP352 vectors, which
    /// state the exact label set per case. Wallet code should use [`Self::new`].
    pub fn with_exact_labels(keys: &ReceiverKeys, labels: &[u32]) -> Result<Self, ProtocolError> {
        Self::build(keys, labels.iter().copied().collect())
    }

    fn build(keys: &ReceiverKeys, labels: BTreeSet<u32>) -> Result<Self, ProtocolError> {
        let secp = Secp256k1::new();
        // The network only affects the textual encoding, never the keys.
        let code = keys.code(Network::Regtest);
        let neg_spend = code.spend.negate(&secp);

        let mut lookup = BTreeMap::<PublicKey, (Scalar, u32)>::new();
        for &m in &labels {
            // Same construction as upstream's `examples/send_and_scan.rs`.
            let label = SilentPaymentCode::get_label(keys.scan_sk, m);
            let labelled = code
                .add_label(label)
                .map_err(|e| ProtocolError::Label(e.to_string()))?;
            let label_point = labelled
                .spend
                .combine(&neg_spend)
                .map_err(|e| ProtocolError::Label(e.to_string()))?;
            lookup.insert(label_point, (label, m));
        }

        Ok(Self {
            inner: Scanner::new(keys.scan_sk, keys.spend_pk, lookup),
            labels,
        })
    }

    /// Label integers this scanner checks for.
    pub fn labels(&self) -> impl Iterator<Item = u32> + '_ {
        self.labels.iter().copied()
    }

    /// Decides whether BIP352 requires `tx` to be scanned, without using any
    /// private key. `prevouts[i]` must be the output spent by `tx.input[i]`.
    pub fn eligibility(
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<Result<(), SkipReason>, ProtocolError> {
        if tx.is_coinbase() {
            return Ok(Err(SkipReason::Coinbase));
        }
        if prevouts.len() != tx.input.len() {
            return Err(ProtocolError::PrevoutCountMismatch {
                inputs: tx.input.len(),
                prevouts: prevouts.len(),
            });
        }
        if !tx.output.iter().any(|o| o.script_pubkey.is_p2tr()) {
            return Ok(Err(SkipReason::NoTaprootOutputs));
        }
        let spends_unknown_segwit = prevouts.iter().any(|p| {
            p.script_pubkey
                .witness_version()
                .is_some_and(|v| v.to_num() > 1)
        });
        if spends_unknown_segwit {
            return Ok(Err(SkipReason::SpendsUnknownSegwitVersion));
        }
        let has_eligible_input = catch_unwind(AssertUnwindSafe(|| {
            tx.input.iter().zip(prevouts).any(|(txin, prevout)| {
                extract_pubkey(txin.clone(), &prevout.script_pubkey).is_some()
            })
        }))
        .map_err(|_| ProtocolError::UpstreamPanic)?;
        if !has_eligible_input {
            return Ok(Err(SkipReason::NoEligibleInputs));
        }
        Ok(Ok(()))
    }

    /// Scans one transaction. `prevouts[i]` must be the output spent by
    /// `tx.input[i]`.
    pub fn scan_tx(
        &self,
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<TxScanOutcome, ProtocolError> {
        if let Err(reason) = Self::eligibility(tx, prevouts)? {
            return Ok(TxScanOutcome::NotEligible(reason));
        }

        let scanned = catch_unwind(AssertUnwindSafe(|| self.inner.scan_tx(tx, prevouts)))
            .map_err(|_| ProtocolError::UpstreamPanic)?;

        match scanned {
            Ok(matches) => Ok(TxScanOutcome::Scanned { matches }),
            // `PublicKey::combine_keys` reports a sum equal to the point at
            // infinity with this exact error. The "no keys at all" case, which
            // yields the same error, was excluded by the eligibility check.
            Err(SpReceiveError::Secp256k1Error(secp256k1::Error::InvalidPublicKeySum)) => Ok(
                TxScanOutcome::NotEligible(SkipReason::InputKeySumIsInfinity),
            ),
            Err(SpReceiveError::ScanLimitExceeded(_)) => self.scan_tx_capped_at_k_max(tx, prevouts),
            Err(other) => Err(ProtocolError::Upstream(other.to_string())),
        }
    }

    /// BIP352 says "If k == K_max, stop scanning": a transaction with more
    /// than K_max matching outputs yields exactly the matches for
    /// `k = 0..K_max`. The pinned upstream scanner instead returns an error
    /// and no matches (docs/DECISIONS.md D-005).
    ///
    /// To get the BIP352 result without re-implementing any matching, the
    /// outputs that would match at `k = K_max` are blanked in a copy of the
    /// transaction (positions are kept, so `vout` stays valid) and the
    /// unmodified upstream scanner is run again. It then stops by itself after
    /// `k = K_max - 1`. Only upstream functions touch keys.
    fn scan_tx_capped_at_k_max(
        &self,
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<TxScanOutcome, ProtocolError> {
        let rescanned = catch_unwind(AssertUnwindSafe(|| {
            let tweak = bdk_sp::receive::compute_tweak_data(tx, prevouts)?;
            let beyond_limit = self.inner.get_spks_from_tweak(&tweak, K_MAX);
            let mut capped = tx.clone();
            for output in &mut capped.output {
                if beyond_limit.contains(&output.script_pubkey) {
                    output.script_pubkey = ScriptBuf::new();
                }
            }
            self.inner.scan_tx(&capped, prevouts)
        }))
        .map_err(|_| ProtocolError::UpstreamPanic)?;

        match rescanned {
            Ok(mut matches) => {
                // The copy has a different txid; the output positions do not.
                let txid = tx.compute_txid();
                for m in &mut matches {
                    m.outpoint.txid = txid;
                }
                if matches.len() != K_MAX as usize {
                    return Err(ProtocolError::KMaxFallbackInconsistent(matches.len()));
                }
                Ok(TxScanOutcome::Scanned { matches })
            }
            Err(SpReceiveError::ScanLimitExceeded(k_max)) => {
                Err(ProtocolError::KMaxExceeded(k_max))
            }
            Err(other) => Err(ProtocolError::Upstream(other.to_string())),
        }
    }

    /// BIP352 public tweak data (`input_hash·A`) of an eligible transaction.
    /// Uses no private key. Returns `None` when the transaction is not eligible.
    pub fn public_tweak_data(
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<Result<PublicKey, SkipReason>, ProtocolError> {
        if let Err(reason) = Self::eligibility(tx, prevouts)? {
            return Ok(Err(reason));
        }
        let computed = catch_unwind(AssertUnwindSafe(|| {
            bdk_sp::receive::compute_tweak_data(tx, prevouts)
        }))
        .map_err(|_| ProtocolError::UpstreamPanic)?;
        match computed {
            Ok(tweak) => Ok(Ok(tweak)),
            Err(SpReceiveError::Secp256k1Error(secp256k1::Error::InvalidPublicKeySum)) => {
                Ok(Err(SkipReason::InputKeySumIsInfinity))
            }
            Err(other) => Err(ProtocolError::Upstream(other.to_string())),
        }
    }
}

impl fmt::Debug for ReceiveScanner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceiveScanner")
            .field("keys", &"<redacted>")
            .field("labels", &self.labels)
            .finish()
    }
}
