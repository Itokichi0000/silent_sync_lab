//! Range scanning.
//!
//! Phase 1 scope: scan one contiguous block range from one source and report
//! what was detected. There is no persistence, resume or reorg handling yet
//! (Phase 3); the function is therefore called `scan_range`, not `sync`.
//!
//! The engine does not trust a source to honour its own contract: a batch that
//! is short, out of order, duplicated or not hash-linked is an error, and any
//! per-transaction error aborts the whole range. Nothing is ever reported as
//! scanned unless every block of the requested range was processed.

use crate::{
    bitcoin::Txid,
    models::{BlockRef, DetectedOutput, MatchProvenance},
    network::ChainIdentity,
    protocol::{ProtocolError, ReceiveScanner, SkipReason, TxScanOutcome},
    source::{BlockEvidence, Evidence, ScanSource, SourceError},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    #[error(
        "chain mismatch: wallet expects {expected:?} but source `{backend_id}` serves {actual:?}"
    )]
    ChainMismatch {
        backend_id: String,
        expected: ChainIdentity,
        actual: ChainIdentity,
    },
    #[error("invalid range {from}..={to}")]
    InvalidRange { from: u32, to: u32 },
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error("source `{backend_id}` returned an inconsistent batch: {reason}")]
    InconsistentBatch { backend_id: String, reason: String },
    #[error("block {height}: transaction {txid}: {source}")]
    Transaction {
        height: u32,
        txid: Txid,
        #[source]
        source: ProtocolError,
    },
}

/// Result of scanning one complete range. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    pub chain: ChainIdentity,
    pub backend_id: String,
    pub first_block: BlockRef,
    pub last_block: BlockRef,
    /// Label integers that were scanned for (always includes the change label
    /// when the scanner was built with [`ReceiveScanner::new`]).
    pub labels_scanned: Vec<u32>,
    pub blocks_scanned: u64,
    pub txs_seen: u64,
    /// Transactions BIP352 required us to scan.
    pub txs_scanned: u64,
    /// Transactions BIP352 told us not to scan, by reason.
    pub txs_skipped: BTreeMap<&'static str, u64>,
    /// Detected outputs, ordered by (height, position in block, vout).
    /// Detection is not a balance and says nothing about spent status.
    pub detected_received_outputs: Vec<DetectedOutput>,
}

/// Scans `from..=to` on `source` for outputs belonging to `scanner`'s receiver.
pub fn scan_range(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    expected_chain: ChainIdentity,
    from: u32,
    to: u32,
) -> Result<ScanReport, ScanError> {
    let backend_id = source.backend_id().to_string();
    let chain = source.chain();
    if chain != expected_chain {
        return Err(ScanError::ChainMismatch {
            backend_id,
            expected: expected_chain,
            actual: chain,
        });
    }
    if from > to {
        return Err(ScanError::InvalidRange { from, to });
    }

    let batch = source.fetch_range(from, to)?;
    check_batch(&backend_id, &batch.blocks, from, to)?;

    let (Some(first), Some(last)) = (batch.blocks.first(), batch.blocks.last()) else {
        // `check_batch` guarantees `to - from + 1 >= 1` blocks.
        return Err(ScanError::InconsistentBatch {
            backend_id,
            reason: "empty batch".to_string(),
        });
    };
    let (first_block, last_block) = (first.block, last.block);

    let mut counters = ScanCounters::default();
    let detected = scan_blocks(&backend_id, chain, scanner, &batch.blocks, &mut counters)?;

    Ok(ScanReport {
        chain,
        backend_id,
        first_block,
        last_block,
        labels_scanned: scanner.labels().collect(),
        blocks_scanned: counters.blocks,
        txs_seen: counters.txs_seen,
        txs_scanned: counters.txs_scanned,
        txs_skipped: counters.txs_skipped,
        detected_received_outputs: detected,
    })
}

/// Counters accumulated while scanning blocks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ScanCounters {
    pub blocks: u64,
    pub txs_seen: u64,
    pub txs_scanned: u64,
    pub txs_skipped: BTreeMap<&'static str, u64>,
}

/// Scans already-fetched blocks and returns what was detected in them,
/// ordered by (height, position in block, vout). Any error aborts the whole
/// call: callers must not record a block as scanned unless this returned `Ok`.
pub(crate) fn scan_blocks(
    backend_id: &str,
    chain: ChainIdentity,
    scanner: &ReceiveScanner,
    blocks: &[BlockEvidence],
    counters: &mut ScanCounters,
) -> Result<Vec<DetectedOutput>, ScanError> {
    let mut detected = Vec::new();
    let mut seen_txids = BTreeSet::<Txid>::new();

    for block in blocks {
        let provenance = MatchProvenance {
            backend_id: backend_id.to_string(),
            evidence_kind: block.evidence.kind(),
        };
        match &block.evidence {
            Evidence::FullBlock(txs) => {
                check_full_block(backend_id, block.block.height, txs, &mut seen_txids)?;
                for entry in txs {
                    counters.txs_seen += 1;
                    let outcome =
                        scanner
                            .scan_tx(&entry.tx, &entry.prevouts)
                            .map_err(|source| ScanError::Transaction {
                                height: block.block.height,
                                txid: entry.tx.compute_txid(),
                                source,
                            })?;
                    match outcome {
                        TxScanOutcome::NotEligible(reason) => {
                            *counters
                                .txs_skipped
                                .entry(SkipReason::as_str(reason))
                                .or_default() += 1;
                        }
                        TxScanOutcome::Scanned { mut matches } => {
                            counters.txs_scanned += 1;
                            matches.sort_by_key(|m| m.outpoint.vout);
                            for m in matches {
                                // The match also carries the output tweak; it
                                // is dropped here on purpose.
                                detected.push(DetectedOutput {
                                    network: chain.network(),
                                    block: block.block,
                                    txid: m.outpoint.txid,
                                    vout: m.outpoint.vout,
                                    value_sats: m.amount.to_sat(),
                                    label: m.label,
                                    provenance: provenance.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        counters.blocks += 1;
    }
    Ok(detected)
}

/// Structural facts every real block satisfies, checked regardless of what the
/// source claims to have verified: at least one transaction, exactly one
/// coinbase and it comes first, and no transaction appears twice in the range.
///
/// What this cannot catch is a block from which a non-coinbase transaction was
/// removed; only a source that verifies the merkle root can (see
/// `SourceCapabilities::verifies_merkle_root`).
fn check_full_block(
    backend_id: &str,
    height: u32,
    txs: &[crate::source::TxWithPrevouts],
    seen_txids: &mut BTreeSet<Txid>,
) -> Result<(), ScanError> {
    let inconsistent = |reason: String| ScanError::InconsistentBatch {
        backend_id: backend_id.to_string(),
        reason,
    };
    if txs.is_empty() {
        return Err(inconsistent(format!("block {height} has no transactions")));
    }
    for (position, entry) in txs.iter().enumerate() {
        if entry.tx.is_coinbase() != (position == 0) {
            return Err(inconsistent(format!(
                "block {height}: transaction at position {position} violates \
                 \"exactly one coinbase, first\""
            )));
        }
        let txid = entry.tx.compute_txid();
        if !seen_txids.insert(txid) {
            return Err(inconsistent(format!(
                "block {height}: transaction {txid} appears more than once in the range"
            )));
        }
    }
    Ok(())
}

/// Verifies that a batch is exactly `from..=to`, ascending and hash-linked.
pub(crate) fn check_batch(
    backend_id: &str,
    blocks: &[BlockEvidence],
    from: u32,
    to: u32,
) -> Result<(), ScanError> {
    let inconsistent = |reason: String| ScanError::InconsistentBatch {
        backend_id: backend_id.to_string(),
        reason,
    };

    let expected_len = u64::from(to - from) + 1;
    if blocks.len() as u64 != expected_len {
        return Err(inconsistent(format!(
            "requested {expected_len} blocks ({from}..={to}) but received {}",
            blocks.len()
        )));
    }
    let mut previous: Option<BlockRef> = None;
    for (offset, entry) in blocks.iter().enumerate() {
        let block = entry.block;
        let expected_height = u64::from(from) + offset as u64;
        if u64::from(block.height) != expected_height {
            return Err(inconsistent(format!(
                "position {offset} holds height {} but {expected_height} was expected",
                block.height
            )));
        }
        if block.hash == block.prev_hash {
            return Err(inconsistent(format!(
                "block {} claims to be its own parent",
                block.height
            )));
        }
        if let Some(prev) = previous {
            if block.prev_hash != prev.hash {
                return Err(inconsistent(format!(
                    "block {} does not link to block {} ({} != {})",
                    block.height, prev.height, block.prev_hash, prev.hash
                )));
            }
        }
        previous = Some(block);
    }
    Ok(())
}
