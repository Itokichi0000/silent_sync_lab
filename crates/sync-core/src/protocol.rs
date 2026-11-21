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
        hashes::{sha256, Hash, HashEngine},
        key::Secp256k1,
        script::Instruction,
        secp256k1::{self, PublicKey, Scalar, SecretKey},
        Amount, Network, OutPoint, Script, ScriptBuf, Transaction, TxIn, TxOut,
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
    pub fn address(&self, network: LabNetwork) -> LabAddress {
        LabAddress {
            network,
            scan: self.scan_public_key(),
            spend: self.spend_pk,
        }
    }

    /// The labelled silent payment address for label integer `m`.
    pub fn labelled_address(
        &self,
        network: LabNetwork,
        m: u32,
    ) -> Result<LabAddress, ProtocolError> {
        let label = SilentPaymentCode::get_label(self.scan_sk, m);
        let labelled = self
            .code(network.to_bitcoin())
            .add_label(label)
            .map_err(|e| ProtocolError::Label(e.to_string()))?;
        Ok(LabAddress {
            network,
            scan: labelled.scan,
            spend: labelled.spend,
        })
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

/// A silent payment address on a lab network.
///
/// The upstream address type has a public, mutable `network` field, so handing
/// it out would let a caller re-encode it for mainnet. This type keeps the
/// network private and can only be created for a [`LabNetwork`]; its `Display`
/// output therefore always carries a test-network prefix.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct LabAddress {
    network: LabNetwork,
    scan: PublicKey,
    spend: PublicKey,
}

impl LabAddress {
    pub fn network(&self) -> LabNetwork {
        self.network
    }

    pub fn scan_public_key(&self) -> PublicKey {
        self.scan
    }

    /// Spend public key; for a labelled address this already includes the label.
    pub fn spend_public_key(&self) -> PublicKey {
        self.spend
    }
}

impl fmt::Display for LabAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        SilentPaymentCode::new_v0(self.scan, self.spend, self.network.to_bitcoin()).fmt(f)
    }
}

impl fmt::Debug for LabAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LabAddress({self})")
    }
}

/// One output that matched the receiver keys.
///
/// The upstream match type is serializable including the per-output tweak.
/// This type is deliberately not `Serialize`, redacts the tweak in `Debug` and
/// only releases it through an explicitly named method.
#[derive(Clone, PartialEq, Eq)]
pub struct ScanMatch {
    pub outpoint: OutPoint,
    pub script_pubkey: ScriptBuf,
    pub amount: Amount,
    /// BIP352 label integer, `None` for the unlabelled address.
    pub label: Option<u32>,
    tweak: SecretKey,
}

impl ScanMatch {
    /// The output tweak `t_k` (plus the label tweak for labelled outputs).
    ///
    /// **Sensitive**: together with the spend private key it yields the
    /// output's private key, and on its own it links the output to the
    /// receiver. Nothing in this lab stores, logs or exports it; it exists so
    /// conformance tests can compare against the BIP352 vectors.
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
            .field("tweak", &"<redacted>")
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
    Scanned { matches: Vec<ScanMatch> },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    #[error("transaction has {inputs} inputs but {prevouts} previous outputs were supplied")]
    PrevoutCountMismatch { inputs: usize, prevouts: usize },
    #[error(
        "input {input} spends a SegWit output but carries no witness; the data was stripped of \
         witnesses or is not consensus-valid, and scanning it would silently miss payments"
    )]
    MissingWitness { input: usize },
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

    /// BIP352 public tweak data (`input_hash·A`) of an eligible transaction.
    /// Uses no private key. The inner `Err` carries the reason when BIP352 says
    /// the transaction must not be scanned.
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
