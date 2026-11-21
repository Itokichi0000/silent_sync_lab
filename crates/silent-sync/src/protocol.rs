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
        absolute::LockTime,
        hashes::{sha256, Hash, HashEngine},
        key::Secp256k1,
        script::Instruction,
        secp256k1::{self, PublicKey, Scalar, SecretKey},
        transaction::Version,
        Amount, Network as BitcoinNetwork, OutPoint, Script, ScriptBuf, Transaction, TxIn, TxOut,
    },
    network::Network,
    source::IndexedTx,
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

/// Declares a method that is public with the `low-level` feature and
/// crate-internal without it. The per-transaction entry points are building
/// blocks: the supported way in is [`scan_range`](crate::engine::scan_range)
/// or [`sync`](crate::sync::sync), which add the checks a single transaction
/// cannot carry (block structure, completeness of a range, duplicates).
macro_rules! low_level {
    ($(#[$meta:meta])* fn $($rest:tt)*) => {
        #[cfg(feature = "low-level")]
        $(#[$meta])*
        pub fn $($rest)*
        #[cfg(not(feature = "low-level"))]
        $(#[$meta])*
        pub(crate) fn $($rest)*
    };
}

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
    /// Builds the receiver's keys from the scan *private* key and the spend
    /// *public* key. The spend private key is never needed for detection.
    pub fn new(scan_sk: SecretKey, spend_pk: PublicKey) -> Self {
        Self { scan_sk, spend_pk }
    }

    /// The scan public key (part of the address).
    pub fn scan_public_key(&self) -> PublicKey {
        self.scan_sk.public_key(&Secp256k1::new())
    }

    /// The spend public key (part of the address).
    pub fn spend_public_key(&self) -> PublicKey {
        self.spend_pk
    }

    /// A stable public identifier for this receiver, derived from its two
    /// public keys. It is not secret, but it is the same on every network and
    /// in every store, so it links them; do not publish it next to a
    /// transaction history.
    pub fn wallet_id(&self) -> String {
        let mut engine = sha256::Hash::engine();
        engine.input(b"silent-sync-lab/wallet-id/v1");
        engine.input(&self.scan_public_key().serialize());
        engine.input(&self.spend_pk.serialize());
        let digest = sha256::Hash::from_engine(engine);
        digest.to_string()[..32].to_string()
    }

    /// The unlabelled silent payment address for `network`.
    pub fn address(&self, network: Network) -> SilentPaymentAddress {
        SilentPaymentAddress {
            network,
            scan: self.scan_public_key(),
            spend: self.spend_pk,
        }
    }

    /// The labelled silent payment address for label integer `m`.
    pub fn labelled_address(
        &self,
        network: Network,
        m: u32,
    ) -> Result<SilentPaymentAddress, ProtocolError> {
        let label = SilentPaymentCode::get_label(self.scan_sk, m);
        let labelled = self
            .code(network.to_bitcoin())
            .add_label(label)
            .map_err(|e| ProtocolError::Label(e.to_string()))?;
        Ok(SilentPaymentAddress {
            network,
            scan: labelled.scan,
            spend: labelled.spend,
        })
    }

    fn code(&self, network: BitcoinNetwork) -> SilentPaymentCode {
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

/// A silent payment address on a lab network.
///
/// The upstream address type has a public, mutable `network` field, so handing
/// it out would let a caller re-encode it for mainnet. This type keeps the
/// network private and can only be created for a [`Network`]; its `Display`
/// output therefore always carries a test-network prefix.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SilentPaymentAddress {
    network: Network,
    scan: PublicKey,
    spend: PublicKey,
}

impl SilentPaymentAddress {
    /// Network the address is encoded for.
    pub fn network(&self) -> Network {
        self.network
    }

    /// The scan public key encoded in the address.
    pub fn scan_public_key(&self) -> PublicKey {
        self.scan
    }

    /// Spend public key; for a labelled address this already includes the label.
    pub fn spend_public_key(&self) -> PublicKey {
        self.spend
    }

    /// Reads an address from its text form and checks that it is for
    /// `network`.
    ///
    /// The prefix decides: `sprt` for regtest, `tsp` for the test networks
    /// (signet here). A mainnet address (`sp`) is refused for every
    /// `network`, since [`Network`] has no mainnet.
    pub fn parse(text: &str, network: Network) -> Result<Self, ProtocolError> {
        let code = SilentPaymentCode::try_from(text)
            .map_err(|e| ProtocolError::Address(format!("not a silent payment address: {e}")))?;
        if code.version() != 0 {
            return Err(ProtocolError::Address(format!(
                "silent payment address version {} is not supported",
                code.version()
            )));
        }
        if !code.is_valid_for_network(network.to_bitcoin()) {
            return Err(ProtocolError::Address(format!(
                "the address is not a {network} address"
            )));
        }
        Ok(Self {
            network,
            scan: code.scan,
            spend: code.spend,
        })
    }
}

impl fmt::Display for SilentPaymentAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        SilentPaymentCode::new_v0(self.scan, self.spend, self.network.to_bitcoin()).fmt(f)
    }
}

impl fmt::Debug for SilentPaymentAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SilentPaymentAddress({self})")
    }
}

/// What scanning one transaction yields. Public only with the `low-level`
/// feature; without it these types are internal to the crate.
mod tx_level {
    use super::*;

    /// One output that matched the receiver keys.
    ///
    /// The upstream match type is serializable including the per-output tweak.
    /// This type is deliberately not `Serialize` and, unless the
    /// `secret-output-tweaks` or the `spend` feature is enabled, **does not
    /// hold the tweak at all**: the scan result that leaves this crate says which output matched,
    /// not how to spend it (DECISIONS D-039).
    #[derive(Clone, PartialEq, Eq)]
    #[non_exhaustive]
    pub struct ScanMatch {
        /// The matching output.
        pub outpoint: OutPoint,
        /// Its script (a taproot output).
        pub script_pubkey: ScriptBuf,
        /// Its value.
        pub amount: Amount,
        /// BIP352 label integer, `None` for the unlabelled address.
        pub label: Option<u32>,
        #[cfg(any(feature = "secret-output-tweaks", feature = "spend"))]
        tweak: SecretKey,
    }

    #[cfg(feature = "spend")]
    impl ScanMatch {
        /// The output tweak, for [`crate::spend::recover_spendable`] only.
        pub(crate) fn tweak_for_spending(&self) -> SecretKey {
            self.tweak
        }
    }

    #[cfg(feature = "secret-output-tweaks")]
    impl ScanMatch {
        /// The output tweak `t_k` (plus the label tweak for labelled outputs).
        ///
        /// **Sensitive**: together with the spend private key it yields the
        /// output's private key, and on its own it links the output to the
        /// receiver. Nothing in this crate stores, logs or exports it. It exists
        /// only with the `secret-output-tweaks` feature, so that conformance
        /// tests can compare against the BIP352 vectors; it is not part of the
        /// SDK's API.
        pub fn sensitive_output_tweak(&self) -> SecretKey {
            self.tweak
        }
    }

    impl From<SpOut> for ScanMatch {
        fn from(spout: SpOut) -> Self {
            Self {
                outpoint: spout.outpoint,
                script_pubkey: spout.script_pubkey,
                amount: spout.amount,
                label: spout.label,
                #[cfg(any(feature = "secret-output-tweaks", feature = "spend"))]
                tweak: spout.tweak,
            }
        }
    }

    impl fmt::Debug for ScanMatch {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("ScanMatch")
                .field("outpoint", &self.outpoint)
                .field("script_pubkey", &self.script_pubkey)
                .field("amount", &self.amount)
                .field("label", &self.label)
                .finish_non_exhaustive()
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
        /// Stable name of the reason, as used in reports.
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
        /// The transaction was scanned. `matches` may be empty.
        Scanned {
            /// Outputs that belong to the receiver.
            matches: Vec<ScanMatch>,
        },
    }
}

#[cfg(not(feature = "low-level"))]
pub(crate) use tx_level::{ScanMatch, SkipReason, TxScanOutcome};
#[cfg(feature = "low-level")]
pub use tx_level::{ScanMatch, SkipReason, TxScanOutcome};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Why a transaction could not be scanned. Unlike a skip, this is an error:
/// the data is unusable or the upstream scanner failed.
pub enum ProtocolError {
    #[error("transaction has {inputs} inputs but {prevouts} previous outputs were supplied")]
    /// The number of previous outputs does not match the number of inputs.
    PrevoutCountMismatch {
        /// Number of inputs of the transaction.
        inputs: usize,
        /// Number of previous outputs that were supplied.
        prevouts: usize,
    },
    #[error(
        "input {input} spends a SegWit output but carries no witness; the data was stripped of \
         witnesses or is not consensus-valid, and scanning it would silently miss payments"
    )]
    /// An input spends a SegWit output but has no witness.
    MissingWitness {
        /// Index of the input without its witness.
        input: usize,
    },
    #[error(
        "more than K_max={0} outputs matched even after capping the scan (docs/DECISIONS.md D-005)"
    )]
    /// More outputs matched than BIP352 allows per recipient group (the limit
    /// is given).
    KMaxExceeded(u32),
    #[error("{0}")]
    /// A silent payment address could not be read, or is for another network.
    Address(String),
    #[error("K_max fallback found {0} matches instead of exactly K_max; refusing to guess")]
    /// The capped rescan did not find exactly the allowed number of matches
    /// (the number found is given).
    KMaxFallbackInconsistent(usize),
    #[error("could not derive label: {0}")]
    /// A label could not be derived.
    Label(String),
    #[error("upstream scanner error: {0}")]
    /// The upstream scanner returned an error.
    Upstream(String),
    #[error("upstream scanner panicked on malformed input; transaction treated as an error")]
    /// The upstream scanner panicked; the panic was contained.
    UpstreamPanic,
}

/// Scanner for one receiver (one scan key / spend key pair plus labels).
pub struct ReceiveScanner {
    inner: Scanner,
    /// Needed for the ECDH step when the tweak data comes from an index.
    scan_sk: SecretKey,
    labels: BTreeSet<u32>,
    /// [`ReceiverKeys::wallet_id`] of the keys this scanner was built from.
    wallet_id: String,
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
    #[cfg(feature = "low-level")]
    pub fn with_exact_labels(keys: &ReceiverKeys, labels: &[u32]) -> Result<Self, ProtocolError> {
        Self::build(keys, labels.iter().copied().collect())
    }

    fn build(keys: &ReceiverKeys, labels: BTreeSet<u32>) -> Result<Self, ProtocolError> {
        let secp = Secp256k1::new();
        // The network only affects the textual encoding, never the keys.
        let code = keys.code(BitcoinNetwork::Regtest);
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
            scan_sk: keys.scan_sk,
            labels,
            wallet_id: keys.wallet_id(),
        })
    }

    /// Public identifier of the receiver this scanner scans for: the
    /// [`ReceiverKeys::wallet_id`] of the keys it was built from. Stores are
    /// bound to it, so a store can only be synced, audited or rescanned with
    /// a scanner for the same receiver.
    pub fn wallet_id(&self) -> &str {
        &self.wallet_id
    }

    /// Label integers this scanner checks for.
    pub fn labels(&self) -> impl Iterator<Item = u32> + '_ {
        self.labels.iter().copied()
    }

    low_level! {
    /// Decides whether BIP352 requires `tx` to be scanned, without using any
    /// private key. `prevouts[i]` must be the output spent by `tx.input[i]`.
    fn eligibility(
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
        // Checked for every transaction, relevant to us or not: witness-less
        // data means the source cannot be used for BIP352 at all.
        if let Some(input) = tx
            .input
            .iter()
            .zip(prevouts)
            .position(|(txin, prevout)| witness_is_missing(txin, &prevout.script_pubkey))
        {
            return Err(ProtocolError::MissingWitness { input });
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
    }

    low_level! {
    /// Scans one transaction. `prevouts[i]` must be the output spent by
    /// `tx.input[i]`.
    fn scan_tx(
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
            Ok(matches) => Ok(TxScanOutcome::Scanned {
                matches: matches.into_iter().map(ScanMatch::from).collect(),
            }),
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
                Ok(TxScanOutcome::Scanned {
                    matches: matches.into_iter().map(ScanMatch::from).collect(),
                })
            }
            Err(SpReceiveError::ScanLimitExceeded(k_max)) => {
                Err(ProtocolError::KMaxExceeded(k_max))
            }
            Err(other) => Err(ProtocolError::Upstream(other.to_string())),
        }
    }

    low_level! {
    /// Scans a transaction as described by a tweak index: public tweak data
    /// plus taproot outputs, no inputs.
    ///
    /// The matching is upstream's `scan_txouts`, run on a stand-in transaction
    /// that holds exactly the listed outputs; the ECDH step is upstream's
    /// `compute_shared_secret`. Nothing here re-implements BIP352, and the
    /// K_max cap is applied the same way as for full transactions.
    ///
    /// What cannot be done here: BIP352's eligibility rules need the inputs,
    /// so whether the transaction should be scanned at all — and whether the
    /// tweak is right, or the transaction even exists — is taken on the
    /// index's word.
    fn scan_indexed_tx(&self, indexed: &IndexedTx) -> Result<Vec<ScanMatch>, ProtocolError> {
        let mut seen = BTreeSet::new();
        if let Some(dup) = indexed.outputs.iter().find(|o| !seen.insert(o.vout)) {
            return Err(ProtocolError::Upstream(format!(
                "index lists output {} of {} twice",
                dup.vout, indexed.txid
            )));
        }
        // Position `i` of the stand-in transaction is `indexed.outputs[i]`.
        let stand_in = |blank: &[ScriptBuf]| Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: Vec::new(),
            output: indexed
                .outputs
                .iter()
                .map(|o| {
                    let mut script = vec![0x51, 0x20];
                    script.extend_from_slice(&o.output_key);
                    let script_pubkey = ScriptBuf::from_bytes(script);
                    TxOut {
                        value: Amount::from_sat(o.value_sats),
                        script_pubkey: if blank.contains(&script_pubkey) {
                            ScriptBuf::new()
                        } else {
                            script_pubkey
                        },
                    }
                })
                .collect(),
        };
        let scan = |blank: &[ScriptBuf]| {
            catch_unwind(AssertUnwindSafe(|| {
                let shared = bdk_sp::compute_shared_secret(&self.scan_sk, &indexed.tweak);
                self.inner.scan_txouts(&stand_in(blank), shared)
            }))
            .map_err(|_| ProtocolError::UpstreamPanic)
        };

        let matches = match scan(&[])? {
            Ok(matches) => matches,
            Err(SpReceiveError::ScanLimitExceeded(k_max)) => {
                // Same correction as `scan_tx_capped_at_k_max` (D-005).
                let beyond_limit = catch_unwind(AssertUnwindSafe(|| {
                    self.inner.get_spks_from_tweak(&indexed.tweak, K_MAX)
                }))
                .map_err(|_| ProtocolError::UpstreamPanic)?;
                match scan(&beyond_limit)? {
                    Ok(matches) if matches.len() == K_MAX as usize => matches,
                    Ok(matches) => {
                        return Err(ProtocolError::KMaxFallbackInconsistent(matches.len()))
                    }
                    Err(SpReceiveError::ScanLimitExceeded(_)) => {
                        return Err(ProtocolError::KMaxExceeded(k_max))
                    }
                    Err(other) => return Err(ProtocolError::Upstream(other.to_string())),
                }
            }
            Err(other) => return Err(ProtocolError::Upstream(other.to_string())),
        };

        matches
            .into_iter()
            .map(|m| {
                let listed = indexed
                    .outputs
                    .get(m.outpoint.vout as usize)
                    .ok_or_else(|| {
                        ProtocolError::Upstream("match outside the listed outputs".to_string())
                    })?;
                let mut found = ScanMatch::from(m);
                found.outpoint = OutPoint::new(indexed.txid, listed.vout);
                Ok(found)
            })
            .collect()
    }
    }

    #[cfg(any(feature = "local-index", feature = "low-level"))]
    low_level! {
    /// BIP352 public tweak data as an index that does **not** apply the
    /// "spends SegWit version > 1" rule would compute it. Exists to model such
    /// indexes (see `source::local_index`); wallets must use
    /// [`Self::public_tweak_data`].
    fn public_tweak_data_ignoring_segwit_version_rule(
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<Result<PublicKey, SkipReason>, ProtocolError> {
        match Self::eligibility(tx, prevouts)? {
            Ok(()) | Err(SkipReason::SpendsUnknownSegwitVersion) => {}
            Err(reason) => return Ok(Err(reason)),
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
        Self::tweak_data_of_eligible(tx, prevouts)
    }
    }

    low_level! {
    /// BIP352 public tweak data (`input_hash·A`) of an eligible transaction.
    ///
    /// Uses no private key. The inner `Err` carries the reason when BIP352 says
    /// the transaction must not be scanned.
    fn public_tweak_data(
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<Result<PublicKey, SkipReason>, ProtocolError> {
        if let Err(reason) = Self::eligibility(tx, prevouts)? {
            return Ok(Err(reason));
        }
        Self::tweak_data_of_eligible(tx, prevouts)
    }
    }

    fn tweak_data_of_eligible(
        tx: &Transaction,
        prevouts: &[TxOut],
    ) -> Result<Result<PublicKey, SkipReason>, ProtocolError> {
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

/// True when `txin` spends a SegWit output that consensus only allows to be
/// spent with a non-empty witness, yet has none.
///
/// Upstream classifies such an input as "not eligible" and moves on. On real
/// chain data that state cannot occur, so it means the witnesses were stripped
/// (e.g. blocks fetched without witness data) — and stripped data makes every
/// P2WPKH / P2TR payment invisible. This inspects scripts only.
///
/// Covered: native P2WPKH, P2WSH, P2TR, and P2SH-wrapped P2WPKH / P2WSH (a
/// scriptSig that is exactly one push of a v0 witness program). Other witness
/// programs, such as pay-to-anchor, may legitimately have an empty witness.
fn witness_is_missing(txin: &TxIn, prevout_script: &Script) -> bool {
    if !txin.witness.is_empty() {
        return false;
    }
    if prevout_script.is_p2wpkh() || prevout_script.is_p2wsh() || prevout_script.is_p2tr() {
        return true;
    }
    if prevout_script.is_p2sh() {
        let mut instructions = txin.script_sig.instructions();
        if let (Some(Ok(Instruction::PushBytes(push))), None) =
            (instructions.next(), instructions.next())
        {
            let redeem = Script::from_bytes(push.as_bytes());
            return redeem.is_p2wpkh() || redeem.is_p2wsh();
        }
    }
    false
}

impl fmt::Debug for ReceiveScanner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReceiveScanner")
            .field("keys", &"<redacted>")
            .field("labels", &self.labels)
            .finish()
    }
}
