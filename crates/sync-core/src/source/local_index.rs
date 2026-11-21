//! A tweak index computed locally from a full-block source.
//!
//! This is the lab's stand-in for an index server ("tweak server"): it turns
//! full blocks into exactly what such a server hands to a light client — per
//! transaction, the BIP352 public tweak data and the taproot outputs — and
//! nothing else. Because it is derived in-process from a source the lab can
//! also scan directly, the two evidence kinds can be compared on identical
//! chains, offline, and deliberate faults can be injected on top of it.
//!
//! It is a model, not a network client: nothing is fetched from a third
//! party, and no claim is made that a real server behaves the same. How a
//! real one (BlindBit Oracle v2) actually behaves is recorded in
//! `docs/RESEARCH.md`; [`IndexPolicy`] exists to reproduce the one difference
//! that was observed.

use super::{
    BlockEvidence, Evidence, IndexedOutput, IndexedTx, ScanBatch, ScanSource, SourceCapabilities,
    SourceError, SourceStats,
};
use crate::{
    models::{BlockRef, EvidenceKind},
    network::ChainIdentity,
    protocol::ReceiveScanner,
};

pub const LOCAL_INDEX_BACKEND_ID: &str = "local_tweak_index";

/// Which transactions the index lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexPolicy {
    /// Exactly the transactions BIP352 says must be scanned.
    Bip352,
    /// Like [`IndexPolicy::Bip352`], but transactions that spend an output
    /// with SegWit version > 1 are listed too. BlindBit Oracle v2 at commit
    /// `f9ab7fd` was observed to behave this way on regtest.
    IgnoreSegwitVersionRule,
}

/// Wraps a full-block source and serves tweak-index evidence derived from it.
pub struct LocalTweakIndex<S> {
    inner: S,
    policy: IndexPolicy,
}

impl<S: ScanSource> LocalTweakIndex<S> {
    pub fn new(inner: S, policy: IndexPolicy) -> Self {
        Self { inner, policy }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: ScanSource> ScanSource for LocalTweakIndex<S> {
    fn backend_id(&self) -> &str {
        LOCAL_INDEX_BACKEND_ID
    }

    fn capabilities(&self) -> SourceCapabilities {
        // What a client of a tweak index can check for itself: nothing about
        // the blocks. That the underlying source checked more is irrelevant
        // to what this evidence kind lets the *scanner* verify.
        SourceCapabilities {
            evidence_kind: EvidenceKind::TweakIndex,
            verifies_merkle_root: false,
            verifies_witness_commitment: false,
            verifies_proof_of_work: false,
            consensus_validated: false,
            remote: false,
        }
    }

    fn chain(&self) -> ChainIdentity {
        self.inner.chain()
    }

    fn tip(&self) -> Result<BlockRef, SourceError> {
        self.inner.tip()
    }

    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        self.inner.block_ref(height)
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        let batch = self.inner.fetch_range(from, to)?;
        let mut blocks = Vec::with_capacity(batch.blocks.len());
        for block in batch.blocks {
            let txs = match block.evidence {
                Evidence::FullBlock(txs) => txs,
                Evidence::TweakIndex(_) => {
                    return Err(SourceError::InvalidData(
                        "a local tweak index needs a full-block source".to_string(),
                    ))
                }
            };
            let mut indexed = Vec::new();
            for entry in &txs {
                let tweak = match self.policy {
                    IndexPolicy::Bip352 => {
                        ReceiveScanner::public_tweak_data(&entry.tx, &entry.prevouts)
                    }
                    IndexPolicy::IgnoreSegwitVersionRule => {
                        ReceiveScanner::public_tweak_data_ignoring_segwit_version_rule(
                            &entry.tx,
                            &entry.prevouts,
                        )
                    }
                }
                .map_err(|e| {
                    SourceError::InvalidData(format!(
                        "block {}: transaction {}: {e}",
                        block.block.height,
                        entry.tx.compute_txid()
                    ))
                })?;
                let Ok(tweak) = tweak else { continue };
                let outputs = entry
                    .tx
                    .output
                    .iter()
                    .enumerate()
                    .filter(|(_, o)| o.script_pubkey.is_p2tr())
                    .filter_map(|(vout, o)| {
                        let output_key: [u8; 32] =
                            o.script_pubkey.as_bytes()[2..].try_into().ok()?;
                        Some(IndexedOutput {
                            vout: u32::try_from(vout).ok()?,
                            value_sats: o.value.to_sat(),
                            output_key,
                        })
                    })
                    .collect();
                indexed.push(IndexedTx {
                    txid: entry.tx.compute_txid(),
                    tweak,
                    outputs,
                });
            }
            blocks.push(BlockEvidence {
                block: block.block,
                evidence: Evidence::TweakIndex(indexed),
            });
        }
        Ok(ScanBatch { blocks })
    }

    fn stats(&self) -> SourceStats {
        self.inner.stats()
    }
}
