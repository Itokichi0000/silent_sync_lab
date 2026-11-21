//! Range scanning.
//!
//! [`scan_range`] is the stateless scan: one contiguous block range from one
//! source, nothing stored. The persistent, resumable variant is `crate::sync`
//! and builds on the same block scanning code.
//!
//! The engine does not trust a source to honour its own contract: a batch that
//! is short, out of order, duplicated or not hash-linked is an error, and any
//! per-transaction error aborts the whole range. Nothing is ever reported as
//! scanned unless every block of the requested range was processed.

use crate::{
    bitcoin::{OutPoint, Txid},
    models::{BlockRef, DetectedOutput, EvidenceKind, MatchProvenance, SpentOutput},
    network::ChainIdentity,
    protocol::{ProtocolError, ReceiveScanner, SkipReason, TxScanOutcome},
    source::{BlockEvidence, Evidence, ScanSource, SourceError},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Why a scan failed. A failure always covers the whole requested range:
/// nothing of it is reported as scanned.
pub enum ScanError {
    #[error(
        "chain mismatch: wallet expects {expected:?} but source `{backend_id}` serves {actual:?}"
    )]
    /// The source serves another chain than the caller expects.
    ChainMismatch {
        /// Backend that was asked.
        backend_id: String,
        /// Chain the caller expects.
        expected: ChainIdentity,
        /// Chain the source reports.
        actual: ChainIdentity,
    },
    #[error("invalid range {from}..={to}")]
    /// The requested range is empty or reversed.
    InvalidRange {
        /// First requested height.
        from: u32,
        /// Last requested height.
        to: u32,
    },
    #[error(transparent)]
    /// The source could not serve the range.
    Source(#[from] SourceError),
    #[error("source `{backend_id}` returned an inconsistent batch: {reason}")]
    /// What the source returned is not the requested range: blocks missing,
    /// repeated or not linked, a transaction listed twice, or an index claim
    /// that no block can satisfy.
    InconsistentBatch {
        /// Backend that returned it.
        backend_id: String,
        /// What was wrong.
        reason: String,
    },
    #[error("block {height}: transaction {txid}: {source}")]
    /// One transaction could not be scanned; the range fails as a whole.
    Transaction {
        /// Height of the block that contains it.
        height: u32,
        /// The transaction.
        txid: Txid,
        #[source]
        /// What went wrong.
        source: ProtocolError,
    },
}

/// Result of scanning one complete range. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ScanReport {
    /// Chain that was scanned.
    pub chain: ChainIdentity,
    /// Backend the data came from.
    pub backend_id: String,
    /// What kind of evidence the detections rest on. For
    /// [`EvidenceKind::TweakIndex`], transaction ids, output positions and
    /// amounts are the index's claims and were not checked against a block.
    pub evidence_kind: EvidenceKind,
    /// First block of the range.
    pub first_block: BlockRef,
    /// Last block of the range.
    pub last_block: BlockRef,
    /// Label integers that were scanned for (always includes the change label
    /// when the scanner was built with [`ReceiveScanner::new`]).
    pub labels_scanned: Vec<u32>,
    /// Number of blocks scanned.
    pub blocks_scanned: u64,
    /// Transactions looked at. For a tweak index: the entries the index listed.
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

    // Fetched and scanned in chunks so that memory use does not grow with the
    // size of the range; the result is all-or-nothing regardless.
    let mut counters = ScanCounters::default();
    let mut detected = Vec::new();
    let mut seen_txids = BTreeSet::new();
    let mut first_block: Option<BlockRef> = None;
    let mut previous: Option<BlockRef> = None;
    let mut next = from;
    loop {
        let last = to.min(next.saturating_add(SCAN_CHUNK_BLOCKS - 1));
        let batch = source.fetch_range(next, last)?;
        check_batch(&backend_id, &batch.blocks, next, last)?;
        if let (Some(prev), Some(first)) = (previous, batch.blocks.first()) {
            if first.block.prev_hash != prev.hash {
                return Err(ScanError::InconsistentBatch {
                    backend_id,
                    reason: format!(
                        "block {} does not link to block {} (the chain changed during the scan?)",
                        first.block.height, prev.height
                    ),
                });
            }
        }
        detected.extend(scan_blocks(
            &backend_id,
            chain,
            scanner,
            &batch.blocks,
            &mut counters,
            &mut seen_txids,
        )?);
        first_block = first_block.or(batch.blocks.first().map(|b| b.block));
        previous = batch.blocks.last().map(|b| b.block);
        if last == to {
            break;
        }
        next = last + 1;
    }
    let (Some(first_block), Some(last_block)) = (first_block, previous) else {
        // `check_batch` guarantees at least one block per chunk.
        return Err(ScanError::InconsistentBatch {
            backend_id,
            reason: "empty batch".to_string(),
        });
    };

    Ok(ScanReport {
        chain,
        backend_id,
        evidence_kind: source.capabilities().evidence_kind,
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

/// Blocks fetched per request by [`scan_range`].
pub const SCAN_CHUNK_BLOCKS: u32 = 100;

/// Counters accumulated while scanning blocks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct ScanCounters {
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
    seen_txids: &mut BTreeSet<Txid>,
) -> Result<Vec<DetectedOutput>, ScanError> {
    let mut detected = Vec::new();

    for block in blocks {
        let provenance = MatchProvenance {
            backend_id: backend_id.to_string(),
            evidence_kind: block.evidence.kind(),
        };
        match &block.evidence {
            Evidence::FullBlock(txs) => {
                check_full_block(backend_id, block.block.height, txs, seen_txids)?;
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
            Evidence::TweakIndex(indexed) => {
                check_index_entries(backend_id, block.block.height, indexed)?;
                for entry in indexed {
                    if !seen_txids.insert(entry.txid) {
                        return Err(ScanError::InconsistentBatch {
                            backend_id: backend_id.to_string(),
                            reason: format!(
                                "block {}: transaction {} appears more than once in the range",
                                block.block.height, entry.txid
                            ),
                        });
                    }
                    counters.txs_seen += 1;
                    counters.txs_scanned += 1;
                    let mut matches = scanner.scan_indexed_tx(entry).map_err(|source| {
                        ScanError::Transaction {
                            height: block.block.height,
                            txid: entry.txid,
                            source,
                        }
                    })?;
                    matches.sort_by_key(|m| m.outpoint.vout);
                    for m in matches {
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
        counters.blocks += 1;
    }
    Ok(detected)
}

/// Most taproot outputs one block can hold: 4,000,000 weight units at 172
/// per output (8 bytes value, 1 byte length, 34 bytes script), ignoring every
/// other part of the block.
pub const MAX_INDEXED_OUTPUTS_PER_BLOCK: usize = 4_000_000 / 172;

/// Bounds on what an index may claim about one block, checked before any
/// entry is scanned. They cannot make the claims true; they keep impossible
/// ones out of the result and bound the work an index can cause.
///
/// * every listed transaction lists at least one output (an index lists a
///   transaction *because* it has taproot outputs);
/// * no output is worth more than all the coins that can exist;
/// * the block does not list more outputs than a block can hold.
fn check_index_entries(
    backend_id: &str,
    height: u32,
    indexed: &[crate::source::IndexedTx],
) -> Result<(), ScanError> {
    let inconsistent = |reason: String| ScanError::InconsistentBatch {
        backend_id: backend_id.to_string(),
        reason: format!("block {height}: {reason}"),
    };
    let max_value = crate::bitcoin::Amount::MAX_MONEY.to_sat();
    let mut outputs = 0usize;
    for entry in indexed {
        if entry.outputs.is_empty() {
            return Err(inconsistent(format!(
                "index entry {} lists no outputs",
                entry.txid
            )));
        }
        if let Some(output) = entry.outputs.iter().find(|o| o.value_sats > max_value) {
            return Err(inconsistent(format!(
                "index entry {}:{} claims {} sat, more than can exist",
                entry.txid, output.vout, output.value_sats
            )));
        }
        outputs = outputs.saturating_add(entry.outputs.len());
    }
    if outputs > MAX_INDEXED_OUTPUTS_PER_BLOCK {
        return Err(inconsistent(format!(
            "the index lists {outputs} outputs, more than a block can hold"
        )));
    }
    Ok(())
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

/// The detected outputs that transactions in `blocks` spend: every input
/// whose previous output `is_detected`. Only full-block evidence lists the
/// inputs of transactions; a tweak index yields nothing here, which means
/// "unknown", not "unspent".
pub(crate) fn spends_in_blocks(
    blocks: &[BlockEvidence],
    is_detected: &dyn Fn(&OutPoint) -> bool,
) -> Vec<SpentOutput> {
    let mut spends = Vec::new();
    for block in blocks {
        let Evidence::FullBlock(txs) = &block.evidence else {
            continue;
        };
        for entry in txs.iter().filter(|entry| !entry.tx.is_coinbase()) {
            let spending_txid = entry.tx.compute_txid();
            for input in &entry.tx.input {
                if is_detected(&input.previous_output) {
                    spends.push(SpentOutput {
                        txid: input.previous_output.txid,
                        vout: input.previous_output.vout,
                        spent_in: block.block,
                        spending_txid,
                    });
                }
            }
        }
    }
    spends
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
