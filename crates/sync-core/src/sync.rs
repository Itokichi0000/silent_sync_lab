//! Persistent, resumable sync with reorg handling.
//!
//! [`sync`] brings a [`SyncStore`] up to the source's tip:
//!
//! 1. the store's binding (chain, wallet, backend, labels, scanner version)
//!    must match what it is given — otherwise nothing is touched;
//! 2. if the stored checkpoint is no longer on the source's chain, the fork
//!    point is located and everything above it is rolled back (blocks and the
//!    detections found in them);
//! 3. blocks after the checkpoint are fetched and scanned in chunks; each
//!    chunk is committed atomically, so an interruption at any point leaves a
//!    store that a later call simply continues from.
//!
//! Fail-closed rules: a chunk that does not link to the stored checkpoint, a
//! source whose tip is *behind* the checkpoint on the same chain, an explicit
//! target above the tip, and any scan error abort the call. What was committed
//! before stays committed; nothing after it is recorded.

use crate::{
    engine::{check_batch, scan_blocks, ScanCounters, ScanError},
    models::{BlockRef, DetectedOutput},
    network::ChainIdentity,
    protocol::ReceiveScanner,
    source::{ScanSource, SourceError},
    store::{StoreError, StoreMeta, SyncStore, SCANNER_VERSION},
};
use serde::Serialize;
use std::collections::BTreeMap;

/// How a [`sync`] call should proceed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncOptions {
    /// Blocks fetched, scanned and committed per step.
    pub chunk_size: u32,
    /// Stop at this height instead of the source's tip. Must not be above it.
    pub target_height: Option<u32>,
    /// Stop after committing at least this many blocks in this call (whole
    /// chunks are always committed). The store stays resumable.
    pub max_blocks: Option<u64>,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            chunk_size: 100,
            target_height: None,
            max_blocks: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Scan(#[from] ScanError),
    #[error("the store is bound to a different {field}: stored `{stored}`, given `{given}`{hint}")]
    BindingMismatch {
        field: &'static str,
        stored: String,
        given: String,
        hint: &'static str,
    },
    #[error(
        "source `{backend_id}` is behind the stored checkpoint (source tip {source_tip}, \
         checkpoint {checkpoint}) on the same chain; refusing to discard scanned blocks"
    )]
    SourceBehindCheckpoint {
        backend_id: String,
        source_tip: u32,
        checkpoint: u32,
    },
    #[error("target height {target} is above the source's tip {tip}")]
    TargetAboveTip { target: u32, tip: u32 },
    #[error(
        "the chain changed while syncing: block {height} does not build on the stored \
         checkpoint; run sync again"
    )]
    ChainMovedDuringSync { height: u32 },
    #[error("chunk size must be at least 1")]
    InvalidChunkSize,
}

impl From<SourceError> for SyncError {
    fn from(e: SourceError) -> Self {
        SyncError::Scan(ScanError::Source(e))
    }
}

/// A reorganization that [`sync`] resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReorgReport {
    /// The checkpoint that turned out not to be on the source's chain.
    pub abandoned_checkpoint: BlockRef,
    /// Highest height at which the store and the source still agree
    /// (`birthday - 1` when they share nothing).
    pub fork_height: u32,
    pub blocks_rolled_back: u64,
    /// Detections that were in the abandoned blocks. They are re-detected if
    /// the same transactions confirm on the new chain.
    pub detections_removed: Vec<DetectedOutput>,
}

/// Outcome of one [`sync`] call. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncReport {
    pub chain: ChainIdentity,
    pub backend_id: String,
    pub wallet_id: String,
    pub labels_scanned: Vec<u32>,
    pub birthday: u32,
    /// Checkpoint found in the store when the call started.
    pub resumed_from: Option<BlockRef>,
    pub reorg: Option<ReorgReport>,
    pub blocks_scanned: u64,
    pub txs_seen: u64,
    pub txs_scanned: u64,
    pub txs_skipped: BTreeMap<&'static str, u64>,
    /// Detections committed by this call (not the whole history).
    pub new_detections: Vec<DetectedOutput>,
    /// Checkpoint after the call.
    pub checkpoint: Option<BlockRef>,
    /// The source's tip when the call started.
    pub source_tip: BlockRef,
    /// Whether the checkpoint reached `source_tip`. `false` after a
    /// `max_blocks` / `target_height` stop — never because of an error.
    pub caught_up: bool,
}

/// Creates the binding for a new store.
pub fn new_store_meta(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    birthday: u32,
    birthday_basis: &str,
) -> StoreMeta {
    StoreMeta {
        chain: source.chain(),
        wallet_id: wallet_id.to_string(),
        backend_id: source.backend_id().to_string(),
        labels: scanner.labels().collect(),
        birthday,
        birthday_basis: birthday_basis.to_string(),
        scanner_version: SCANNER_VERSION.to_string(),
    }
}

fn check_binding(
    meta: &StoreMeta,
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
) -> Result<(), SyncError> {
    let mismatch = |field, stored: String, given: String, hint| {
        Err(SyncError::BindingMismatch {
            field,
            stored,
            given,
            hint,
        })
    };
    if meta.chain != source.chain() {
        return mismatch(
            "chain",
            meta.chain.network().to_string(),
            source.chain().network().to_string(),
            "",
        );
    }
    if meta.wallet_id != wallet_id {
        return mismatch("wallet", meta.wallet_id.clone(), wallet_id.to_string(), "");
    }
    if meta.backend_id != source.backend_id() {
        return mismatch(
            "backend",
            meta.backend_id.clone(),
            source.backend_id().to_string(),
            "; use a separate store per backend",
        );
    }
    let labels: Vec<u32> = scanner.labels().collect();
    if meta.labels != labels {
        return mismatch(
            "label set",
            format!("{:?}", meta.labels),
            format!("{labels:?}"),
            "; blocks already scanned were not checked for other labels, so a new store \
             (full rescan) is required",
        );
    }
    if meta.scanner_version != SCANNER_VERSION {
        return mismatch(
            "scanner version",
            meta.scanner_version.clone(),
            SCANNER_VERSION.to_string(),
            "; a new store (full rescan) is required",
        );
    }
    Ok(())
}

/// Rolls the store back to the fork point if its checkpoint is not on the
/// source's chain.
fn resolve_reorg(
    source: &dyn ScanSource,
    store: &mut dyn SyncStore,
    meta: &StoreMeta,
    checkpoint: BlockRef,
    source_tip: BlockRef,
) -> Result<Option<ReorgReport>, SyncError> {
    let on_chain = |height: u32, stored: BlockRef| -> Result<bool, SyncError> {
        Ok(source.block_ref(height)?.hash == stored.hash)
    };

    if checkpoint.height <= source_tip.height && on_chain(checkpoint.height, checkpoint)? {
        return Ok(None);
    }
    if checkpoint.height > source_tip.height {
        // The source is shorter. If its tip is one of our stored blocks, the
        // source is merely behind (restarting, still syncing, ...): that is
        // not evidence that the blocks above were reorganized away.
        let stored_at_tip = store.block_at(source_tip.height)?;
        if stored_at_tip.is_some_and(|b| b.hash == source_tip.hash)
            || (source_tip.height < meta.birthday)
        {
            return Err(SyncError::SourceBehindCheckpoint {
                backend_id: source.backend_id().to_string(),
                source_tip: source_tip.height,
                checkpoint: checkpoint.height,
            });
        }
    }

    // Walk down to the highest height where both sides agree.
    let mut height = checkpoint.height.min(source_tip.height);
    let fork_height = loop {
        if height < meta.birthday {
            break meta.birthday - 1;
        }
        let stored = store.block_at(height)?.ok_or_else(|| {
            StoreError::Invariant(format!(
                "no stored block at height {height} below the checkpoint"
            ))
        })?;
        if on_chain(height, stored)? {
            break height;
        }
        height -= 1;
    };

    let before = store.block_count()?;
    let detections_removed = store.rollback_to(fork_height)?;
    let blocks_rolled_back = before - store.block_count()?;
    Ok(Some(ReorgReport {
        abandoned_checkpoint: checkpoint,
        fork_height,
        blocks_rolled_back,
        detections_removed,
    }))
}

/// Brings `store` up to the source's tip (or to the limits in `options`).
pub fn sync(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    store: &mut dyn SyncStore,
    options: &SyncOptions,
) -> Result<SyncReport, SyncError> {
    if options.chunk_size == 0 {
        return Err(SyncError::InvalidChunkSize);
    }
    let meta = store.meta()?.ok_or(StoreError::NotInitialized)?;
    check_binding(&meta, source, scanner, wallet_id)?;
    let backend_id = source.backend_id().to_string();

    let source_tip = source.tip()?;
    let resumed_from = store.checkpoint()?;
    let reorg = match resumed_from {
        Some(checkpoint) => resolve_reorg(source, store, &meta, checkpoint, source_tip)?,
        None => None,
    };

    let target = match options.target_height {
        Some(target) if target > source_tip.height => {
            return Err(SyncError::TargetAboveTip {
                target,
                tip: source_tip.height,
            })
        }
        Some(target) => target,
        None => source_tip.height,
    };

    let mut report = SyncReport {
        chain: meta.chain,
        backend_id: backend_id.clone(),
        wallet_id: meta.wallet_id.clone(),
        labels_scanned: meta.labels.clone(),
        birthday: meta.birthday,
        resumed_from,
        reorg,
        blocks_scanned: 0,
        txs_seen: 0,
        txs_scanned: 0,
        txs_skipped: BTreeMap::new(),
        new_detections: Vec::new(),
        checkpoint: store.checkpoint()?,
        source_tip,
        caught_up: false,
    };

    loop {
        let checkpoint = store.checkpoint()?;
        let next = match checkpoint {
            Some(cp) => match cp.height.checked_add(1) {
                Some(next) => next,
                None => break,
            },
            None => meta.birthday,
        };
        if next > target
            || options
                .max_blocks
                .is_some_and(|max| report.blocks_scanned >= max)
        {
            break;
        }
        let last = target.min(next.saturating_add(options.chunk_size - 1));

        let batch = source.fetch_range(next, last)?;
        check_batch(&backend_id, &batch.blocks, next, last)?;
        if let (Some(cp), Some(first)) = (checkpoint, batch.blocks.first()) {
            if first.block.prev_hash != cp.hash {
                return Err(SyncError::ChainMovedDuringSync { height: next });
            }
        }

        let mut counters = ScanCounters::default();
        let detections = scan_blocks(
            &backend_id,
            meta.chain,
            scanner,
            &batch.blocks,
            &mut counters,
        )?;
        let blocks: Vec<BlockRef> = batch.blocks.iter().map(|b| b.block).collect();
        store.append(&blocks, &detections)?;

        report.blocks_scanned += blocks.len() as u64;
        report.txs_seen += counters.txs_seen;
        report.txs_scanned += counters.txs_scanned;
        for (reason, count) in counters.txs_skipped {
            *report.txs_skipped.entry(reason).or_default() += count;
        }
        report.new_detections.extend(detections);
    }

    report.checkpoint = store.checkpoint()?;
    report.caught_up = report
        .checkpoint
        .is_some_and(|cp| cp.height == source_tip.height && cp.hash == source_tip.hash);
    Ok(report)
}
