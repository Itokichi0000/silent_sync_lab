//! Scan data sources.
//!
//! Different backends can provide *different information* (full blocks with
//! previous outputs, precomputed tweaks, filters, ...). That difference is made
//! explicit through [`SourceCapabilities`] and the [`Evidence`] enum instead of
//! being hidden behind one lowest-common-denominator type (design spec §4.2).
//!
//! Two kinds of evidence exist: full blocks with the outputs they spend
//! ([`Evidence::FullBlock`]) and a per-transaction tweak index
//! ([`Evidence::TweakIndex`]). They are separate variants because they let the
//! scanner verify very different things; see `docs/FAULT_INJECTION.md` for
//! what each can and cannot detect.

#[cfg(feature = "core-rest")]
pub mod bitcoin_core;
#[cfg(feature = "blindbit-http")]
pub mod blindbit;
#[cfg(any(feature = "core-rest", feature = "fixture"))]
mod block_check;
#[cfg(feature = "fixture")]
pub mod fixture;
#[cfg(any(feature = "core-rest", feature = "blindbit-http"))]
mod http;
#[cfg(feature = "local-index")]
pub mod local_index;

#[cfg(feature = "core-rest")]
pub use bitcoin_core::CoreRestSource;
#[cfg(feature = "blindbit-http")]
pub use blindbit::BlindbitHttpSource;

#[cfg(any(feature = "core-rest", feature = "fixture"))]
use crate::bitcoin::script::{self, Instruction};
use crate::{
    bitcoin::{secp256k1::PublicKey, Transaction, TxOut, Txid},
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, NetworkError},
};

/// What a source can and cannot vouch for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceCapabilities {
    /// Evidence kind this source produces.
    pub evidence_kind: EvidenceKind,
    /// The source checks that each block's transactions commit to the merkle
    /// root in its header.
    pub verifies_merkle_root: bool,
    /// The source checks the BIP141 witness commitment. Witness data is what
    /// BIP352 input classification reads, and the header merkle root alone
    /// does not commit to it.
    pub verifies_witness_commitment: bool,
    /// The source checks proof of work of each header against the header's own
    /// `bits`. This does **not** establish that the chain is the most-work
    /// chain of the network.
    pub verifies_proof_of_work: bool,
    /// Transactions and previous outputs were consensus-validated by a full
    /// node the user controls. `false` means the data is only structurally
    /// checked.
    pub consensus_validated: bool,
    /// The data crosses a network boundary to a third party, which then learns
    /// things such as IP address, timing and requested ranges.
    pub remote: bool,
}

/// A transaction together with the outputs its inputs spend.
/// `prevouts[i]` is spent by `tx.input[i]`; it is empty for a coinbase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxWithPrevouts {
    /// The transaction, with witnesses.
    pub tx: Transaction,
    /// The outputs its inputs spend, in input order.
    pub prevouts: Vec<TxOut>,
}

/// A taproot output as listed by a tweak index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedOutput {
    /// Index of the output within its transaction.
    pub vout: u32,
    /// Value in satoshi, as the index states it.
    pub value_sats: u64,
    /// The 32-byte witness program (x-only output key).
    pub output_key: [u8; 32],
}

/// One transaction as listed by a tweak index: its BIP352 public tweak data
/// (`input_hash·A`) and its taproot outputs. Inputs are not available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTx {
    /// Transaction id, as the index states it.
    pub txid: Txid,
    /// BIP352 public tweak data (`input_hash·A`) of the transaction.
    pub tweak: PublicKey,
    /// The taproot outputs the index lists for it.
    pub outputs: Vec<IndexedOutput>,
}

/// Scan data for one block.
///
/// The variants carry *different information* and are deliberately not
/// unified: with [`Evidence::FullBlock`] the client itself decides which
/// transactions BIP352 says to scan and derives the tweak data; with
/// [`Evidence::TweakIndex`] both were decided by whoever built the index.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Evidence {
    /// Every transaction of the block, in block order, with previous outputs.
    FullBlock(Vec<TxWithPrevouts>),
    /// The transactions an index considers scannable, with tweak data.
    TweakIndex(Vec<IndexedTx>),
}

impl Evidence {
    /// The kind of this evidence.
    pub fn kind(&self) -> EvidenceKind {
        match self {
            Evidence::FullBlock(_) => EvidenceKind::FullBlockWithPrevouts,
            Evidence::TweakIndex(_) => EvidenceKind::TweakIndex,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// What a source supplies for one block.
pub struct BlockEvidence {
    /// The block the evidence is for.
    pub block: BlockRef,
    /// The evidence itself.
    pub evidence: Evidence,
}

/// Blocks of a requested range, ascending by height.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanBatch {
    /// One entry per requested height, ascending, without gaps.
    pub blocks: Vec<BlockEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Why a source could not supply what was asked.
pub enum SourceError {
    #[error("invalid range {from}..={to}")]
    /// The requested range is empty or reversed.
    InvalidRange {
        /// First requested height.
        from: u32,
        /// Last requested height.
        to: u32,
    },
    #[error("range {from}..={to} is not fully available (source covers {available_from}..={available_to})")]
    /// The source does not have all of the requested range.
    RangeUnavailable {
        /// First requested height.
        from: u32,
        /// Last requested height.
        to: u32,
        /// First height the source can serve.
        available_from: u32,
        /// Last height the source can serve.
        available_to: u32,
    },
    #[error("source data is invalid: {0}")]
    /// The source returned data that fails validation.
    InvalidData(String),
    #[error("source is unavailable: {0}")]
    /// The source could not be reached or did not answer usefully.
    Unavailable(String),
    #[error(transparent)]
    /// The source is on a network that is refused.
    Network(#[from] NetworkError),
}

/// Transfer counters of a source, for measurements. Offline sources report zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SourceStats {
    /// Requests sent since the source was created.
    pub requests: u64,
    /// Response body bytes received since the source was created.
    pub downloaded_bytes: u64,
}

/// A backend that supplies scan data for block ranges.
///
/// Implementations must be fail-closed: a range that cannot be served
/// completely is an error, never a shorter batch.
pub trait ScanSource {
    /// Stable identifier used in provenance and reports, e.g. `"fixture"`.
    fn backend_id(&self) -> &str;
    /// What this source verifies and what kind of evidence it yields.
    fn capabilities(&self) -> SourceCapabilities;
    /// The chain this source serves.
    fn chain(&self) -> ChainIdentity;
    /// Highest block the source can currently serve.
    fn tip(&self) -> Result<BlockRef, SourceError>;
    /// Scan data for every block in `from..=to`, ascending.
    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError>;
    /// Position of the block at `height` on the source's current chain,
    /// without scan data. Used to locate fork points; backends should
    /// override the default, which fetches the whole block.
    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        self.fetch_range(height, height)?
            .blocks
            .first()
            .map(|b| b.block)
            .ok_or_else(|| {
                SourceError::InvalidData(format!("no block returned for height {height}"))
            })
    }
    /// Block references for `from..=to`. The default asks for them one by
    /// one; sources for which that costs a request each override it.
    fn block_refs(&self, from: u32, to: u32) -> Result<Vec<BlockRef>, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        (from..=to).map(|height| self.block_ref(height)).collect()
    }
    /// Transfer counters; sources without I/O keep the default.
    fn stats(&self) -> SourceStats {
        SourceStats::default()
    }
}

/// Block height committed to by a coinbase transaction (BIP34).
///
/// Unlike `bitcoin::Block::bip34_block_height` this also accepts the
/// `OP_1`..`OP_16` encoding that Bitcoin Core produces for heights 1 to 16,
/// which every regtest chain starts with.
#[cfg(any(feature = "core-rest", feature = "fixture"))]
pub(crate) fn coinbase_height(coinbase: &Transaction) -> Option<u32> {
    let script_sig = &coinbase.input.first()?.script_sig;
    match script_sig.instructions().next()?.ok()? {
        Instruction::PushBytes(bytes) => {
            u32::try_from(script::read_scriptint(bytes.as_bytes()).ok()?).ok()
        }
        Instruction::Op(op) => {
            let code = op.to_u8();
            (0x51..=0x60)
                .contains(&code)
                .then(|| u32::from(code - 0x50))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitcoin::{
        absolute::LockTime, script::Builder, transaction::Version, OutPoint, ScriptBuf, Sequence,
        TxIn, Witness,
    };

    fn coinbase_with(script_sig: ScriptBuf) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig,
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![],
        }
    }

    #[test]
    fn coinbase_height_handles_small_and_large_heights() {
        for height in [1u32, 2, 16, 17, 127, 128, 255, 256, 65_535, 65_536, 900_000] {
            let script_sig = Builder::new()
                .push_int(i64::from(height))
                .push_slice(b"x")
                .into_script();
            assert_eq!(
                coinbase_height(&coinbase_with(script_sig)),
                Some(height),
                "{height}"
            );
        }
    }

    #[test]
    fn coinbase_height_rejects_garbage() {
        assert_eq!(coinbase_height(&coinbase_with(ScriptBuf::new())), None);
        // OP_0 would be height 0 as a push of an empty vector.
        let negative = Builder::new().push_int(-5).into_script();
        assert_eq!(coinbase_height(&coinbase_with(negative)), None);
        // A truncated push.
        assert_eq!(
            coinbase_height(&coinbase_with(ScriptBuf::from_bytes(vec![0x05, 0x01]))),
            None
        );
    }
}
