//! Scan data sources.
//!
//! Different backends can provide *different information* (full blocks with
//! previous outputs, precomputed tweaks, filters, ...). That difference is made
//! explicit through [`SourceCapabilities`] and the [`Evidence`] enum instead of
//! being hidden behind one lowest-common-denominator type (design spec §4.2).
//!
//! Only full-block evidence exists so far. Tweak-index evidence will be added
//! as a *separate* `Evidence` variant together with the first oracle-style
//! backend (Phase 4); it is intentionally not sketched here as a mock.

#[cfg(feature = "core-rest")]
pub mod bitcoin_core;
mod block_check;
pub mod fixture;

use crate::{
    bitcoin::{
        script::{self, Instruction},
        Transaction, TxOut,
    },
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
    pub tx: Transaction,
    pub prevouts: Vec<TxOut>,
}

/// Scan data for one block.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Evidence {
    /// Every transaction of the block, in block order, with previous outputs.
    FullBlock(Vec<TxWithPrevouts>),
}

impl Evidence {
    pub fn kind(&self) -> EvidenceKind {
        match self {
            Evidence::FullBlock(_) => EvidenceKind::FullBlockWithPrevouts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockEvidence {
    pub block: BlockRef,
    pub evidence: Evidence,
}

/// Blocks of a requested range, ascending by height.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanBatch {
    pub blocks: Vec<BlockEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("invalid range {from}..={to}")]
    InvalidRange { from: u32, to: u32 },
    #[error("range {from}..={to} is not fully available (source covers {available_from}..={available_to})")]
    RangeUnavailable {
        from: u32,
        to: u32,
        available_from: u32,
        available_to: u32,
    },
    #[error("source data is invalid: {0}")]
    InvalidData(String),
    #[error("source is unavailable: {0}")]
    Unavailable(String),
    #[error(transparent)]
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
    fn capabilities(&self) -> SourceCapabilities;
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
pub fn coinbase_height(coinbase: &Transaction) -> Option<u32> {
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
