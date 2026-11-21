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
//! A transaction listed twice is refused within one chunk by the scan, and
//! across chunks only when it pays the receiver (the store records an output
//! once). A stateless [`scan_range`](crate::engine::scan_range) checks the
//! whole range.
//!
//! Fail-closed rules: a chunk that does not link to the stored checkpoint, a
//! source whose tip is *behind* the checkpoint on the same chain, an explicit
//! target above the tip, and any scan error abort the call. What was committed
//! before stays committed; nothing after it is recorded.

use crate::{
    engine::{check_batch, scan_blocks, ScanCounters, ScanError},
    models::{BlockRef, DetectedOutput, EvidenceKind},
    network::ChainIdentity,
    protocol::ReceiveScanner,
    source::{ScanSource, SourceError},
    store::{StoreError, StoreMeta, SyncStore, SCANNER_VERSION},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// How a [`sync`] call should proceed.
///
/// Start from [`SyncOptions::default`] and adjust it; more options may be
/// added later, so the struct cannot be built field by field elsewhere.
///
/// ```
/// use silent_sync::sync::SyncOptions;
/// let options = SyncOptions::default()
///     .with_chunk_size(50)
///     .with_target_height(Some(1_000));
/// assert_eq!(options.chunk_size, 50);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyncOptions {
    /// Blocks fetched, scanned and committed per step.
    pub chunk_size: u32,
    /// Stop at this height instead of the source's tip. Must not be above it.
    pub target_height: Option<u32>,
    /// Stop after committing at least this many blocks in this call (whole
    /// chunks are always committed). The store stays resumable.
    pub max_blocks: Option<u64>,
    /// Deepest reorganization that is followed: how many scanned blocks a
    /// call may discard when the source's chain no longer contains the
    /// checkpoint. A source that asks for more is refused and the store is
    /// left as it is. `None` removes the limit.
    ///
    /// The default is [`DEFAULT_MAX_REORG_DEPTH`].
    pub max_reorg_depth: Option<u32>,
}

/// Default for [`SyncOptions::max_reorg_depth`]: 100 blocks, the depth after
/// which Bitcoin itself treats a block as final enough to spend its coinbase.
/// A source that rewrites more than that is far more likely to be wrong, or
/// lying, than to describe a real reorganization.
pub const DEFAULT_MAX_REORG_DEPTH: u32 = 100;

impl SyncOptions {
    /// Blocks fetched, scanned and committed per step (at least 1).
    pub fn with_chunk_size(mut self, chunk_size: u32) -> Self {
        self.chunk_size = chunk_size;
        self
    }

    /// Stop at this height instead of the source's tip (`None`: the tip).
    pub fn with_target_height(mut self, target_height: Option<u32>) -> Self {
        self.target_height = target_height;
        self
    }

    /// Stop after committing at least this many blocks (`None`: no limit).
    pub fn with_max_blocks(mut self, max_blocks: Option<u64>) -> Self {
        self.max_blocks = max_blocks;
        self
    }

    /// Deepest reorganization that is followed (`None`: no limit).
    pub fn with_max_reorg_depth(mut self, max_reorg_depth: Option<u32>) -> Self {
        self.max_reorg_depth = max_reorg_depth;
        self
    }
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            chunk_size: 100,
            target_height: None,
            max_blocks: None,
            max_reorg_depth: Some(DEFAULT_MAX_REORG_DEPTH),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
/// Why a sync call failed.
pub enum SyncError {
    #[error(transparent)]
    /// The store failed or refused an operation.
    Store(#[from] StoreError),
    #[error(transparent)]
    /// Fetching or scanning failed.
    Scan(#[from] ScanError),
    #[error("the store is bound to a different {field}: stored `{stored}`, given `{given}`{hint}")]
    /// The store is bound to something else than what was given.
    BindingMismatch {
        /// Which part of the binding differs.
        field: &'static str,
        /// What the store is bound to.
        stored: String,
        /// What the call was given.
        given: String,
        /// What to do about it.
        hint: &'static str,
    },
    #[error(
        "source `{backend_id}` is behind the stored checkpoint (source tip {source_tip}, \
         checkpoint {checkpoint}) and shows no competing chain; refusing to discard scanned \
         blocks. If the chain was reorganized, sync again once the source has caught up"
    )]
    /// The source's chain ends below the stored checkpoint without
    /// contradicting it.
    SourceBehindCheckpoint {
        /// Backend that was asked.
        backend_id: String,
        /// Height of the source's tip.
        source_tip: u32,
        /// Height of the stored checkpoint.
        checkpoint: u32,
    },
    #[error("target height {target} is above the source's tip {tip}")]
    /// The requested target height is above the source's tip.
    TargetAboveTip {
        /// Requested target height.
        target: u32,
        /// Height of the source's tip.
        tip: u32,
    },
    #[error(
        "the chain changed while syncing: block {height} does not build on the stored \
         checkpoint; run sync again"
    )]
    /// The blocks fetched do not build on the stored checkpoint: the chain
    /// changed between two steps of the call.
    ChainMovedDuringSync {
        /// Height of the first block that does not link.
        height: u32,
    },
    #[error("chunk size must be at least 1")]
    /// `chunk_size` was 0.
    InvalidChunkSize,
    /// Following the source would discard more scanned blocks than
    /// [`SyncOptions::max_reorg_depth`] allows. Nothing was changed.
    #[error(
        "source `{backend_id}` no longer has the stored checkpoint {checkpoint} on its chain, \
         and its chain differs for at least {at_least} blocks below it — more than the limit \
         of {limit}; refusing to discard scanned blocks. If this reorganization is real, \
         raise the limit"
    )]
    ReorgTooDeep {
        /// Backend that was asked.
        backend_id: String,
        /// Height of the stored checkpoint.
        checkpoint: u32,
        /// Number of stored blocks found not to be on the source's chain
        /// when the search was stopped (the fork is at least this deep).
        at_least: u32,
        /// The limit that was exceeded.
        limit: u32,
    },
    /// The call failed after it had already changed the store. `partial`
    /// reports exactly what was changed (a reorg rollback and/or committed
    /// chunks); the store is consistent and can be synced again.
    #[error("sync interrupted after changing the store: {cause}")]
    Interrupted {
        /// The error that stopped the call.
        cause: Box<SyncError>,
        /// What the call had already changed.
        partial: Box<SyncReport>,
    },
}

impl From<SourceError> for SyncError {
    fn from(e: SourceError) -> Self {
        SyncError::Scan(ScanError::Source(e))
    }
}

/// A reorganization that [`sync`] resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct ReorgReport {
    /// The checkpoint that turned out not to be on the source's chain.
    pub abandoned_checkpoint: BlockRef,
    /// Highest height at which the store and the source still agree
    /// (`birthday - 1` when they share nothing).
    pub fork_height: u32,
    /// Number of blocks removed from the store.
    pub blocks_rolled_back: u64,
    /// Detections that were in the abandoned blocks. They are re-detected if
    /// the same transactions confirm on the new chain.
    pub detections_removed: Vec<DetectedOutput>,
}

/// Outcome of one [`sync`] call. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct SyncReport {
    /// Chain that was synced.
    pub chain: ChainIdentity,
    /// Backend the data came from.
    pub backend_id: String,
    /// What kind of evidence the detections rest on (see
    /// [`ScanReport::evidence_kind`](crate::engine::ScanReport)).
    pub evidence_kind: EvidenceKind,
    /// Public identifier of the receiver.
    pub wallet_id: String,
    /// Labels that were scanned for.
    pub labels_scanned: Vec<u32>,
    /// First height the store covers.
    pub birthday: u32,
    /// Checkpoint found in the store when the call started.
    pub resumed_from: Option<BlockRef>,
    /// The reorganization this call resolved, if any.
    pub reorg: Option<ReorgReport>,
    /// Blocks scanned and committed by this call.
    pub blocks_scanned: u64,
    /// Transactions looked at (for a tweak index: entries listed).
    pub txs_seen: u64,
    /// Transactions that were eligible and scanned.
    pub txs_scanned: u64,
    /// Transactions BIP352 says not to scan, by reason.
    pub txs_skipped: BTreeMap<&'static str, u64>,
    /// Detections committed by this call (not the whole history).
    pub new_detections: Vec<DetectedOutput>,
    /// Checkpoint after the call.
    pub checkpoint: Option<BlockRef>,
    /// The source's tip when the call started.
    pub source_tip: BlockRef,
    /// Whether the checkpoint is exactly `source_tip` (same height and hash).
    pub caught_up: bool,
    /// Why the call stopped where it did.
    pub stopped_because: StopReason,
}

/// Why a [`sync`] call stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StopReason {
    /// The checkpoint is the source's tip.
    CaughtUp,
    /// `target_height` was reached; it is below the source's tip.
    TargetReached,
    /// `max_blocks` was reached.
    MaxBlocksReached,
    /// The store's birthday is above the source's tip: nothing to scan yet.
    BirthdayAboveTip,
    /// The source's tip changed while syncing; sync again.
    TipChanged,
    /// The call failed (only in the partial report of
    /// [`SyncError::Interrupted`]).
    Error,
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

/// One part of a store's binding that differs from what a call was given.
pub(crate) struct BindingDifference {
    pub field: &'static str,
    pub stored: String,
    pub given: String,
    pub hint: &'static str,
}

/// Compares a store's binding with what a call was given. `backend_id` is
/// `None` when the call may legitimately use another backend than the one
/// the store was filled from (auditing a store against a reference);
/// `scanner` is `None` when the call may use another label set or scanner
/// version (scanning a store's range again into a new store).
pub(crate) fn binding_difference(
    meta: &StoreMeta,
    chain: ChainIdentity,
    backend_id: Option<&str>,
    scanner: Option<&ReceiveScanner>,
    wallet_id: &str,
) -> Option<BindingDifference> {
    let differs = |field, stored: String, given: String, hint| {
        Some(BindingDifference {
            field,
            stored,
            given,
            hint,
        })
    };
    if meta.chain != chain {
        return differs(
            "chain",
            meta.chain.network().to_string(),
            chain.network().to_string(),
            "",
        );
    }
    if meta.wallet_id != wallet_id {
        return differs("wallet", meta.wallet_id.clone(), wallet_id.to_string(), "");
    }
    if let Some(backend_id) = backend_id {
        if meta.backend_id != backend_id {
            return differs(
                "backend",
                meta.backend_id.clone(),
                backend_id.to_string(),
                "; use a separate store per backend",
            );
        }
    }
    let labels: Vec<u32> = scanner?.labels().collect();
    if meta.labels != labels {
        return differs(
            "label set",
            format!("{:?}", meta.labels),
            format!("{labels:?}"),
            "; blocks already scanned were not checked for other labels, so the stored range \
             has to be scanned again into a new store (`rescan_into`)",
        );
    }
    if meta.scanner_version != SCANNER_VERSION {
        return differs(
            "scanner version",
            meta.scanner_version.clone(),
            SCANNER_VERSION.to_string(),
            "; the stored range has to be scanned again into a new store (`rescan_into`)",
        );
    }
    None
}

fn check_binding(
    meta: &StoreMeta,
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
) -> Result<(), SyncError> {
    match binding_difference(
        meta,
        source.chain(),
        Some(source.backend_id()),
        Some(scanner),
        wallet_id,
    ) {
        Some(d) => Err(SyncError::BindingMismatch {
            field: d.field,
            stored: d.stored,
            given: d.given,
            hint: d.hint,
        }),
        None => Ok(()),
    }
}

/// Rolls the store back to the fork point if its checkpoint is not on the
/// source's chain. Returns the report together with the checkpoint the store
/// has afterwards.
///
/// Everything that can fail is done *before* the rollback; once the store has
/// been changed this function only builds its report, so a rollback can never
/// happen without being returned.
fn resolve_reorg(
    source: &dyn ScanSource,
    store: &mut dyn SyncStore,
    meta: &StoreMeta,
    checkpoint: BlockRef,
    source_tip: BlockRef,
    max_reorg_depth: Option<u32>,
) -> Result<Option<(ReorgReport, Option<BlockRef>)>, SyncError> {
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

    // Walk down to the highest height where both sides agree — but not
    // further than the caller allows: every step down is a scanned block
    // that would be discarded. Checked before each step, so a source cannot
    // make this loop walk the whole store, and before anything is changed.
    let too_deep = |fork_height: u32| {
        let depth = checkpoint.height - fork_height;
        match max_reorg_depth {
            Some(limit) if depth > limit => Err(SyncError::ReorgTooDeep {
                backend_id: source.backend_id().to_string(),
                checkpoint: checkpoint.height,
                at_least: depth,
                limit,
            }),
            _ => Ok(()),
        }
    };
    let mut height = checkpoint.height.min(source_tip.height);
    let fork_height = loop {
        if height < meta.birthday {
            too_deep(meta.birthday - 1)?;
            break meta.birthday - 1;
        }
        too_deep(height)?;
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

    // The block the store will be at afterwards (none if nothing is shared).
    let new_checkpoint = if fork_height >= meta.birthday {
        Some(store.block_at(fork_height)?.ok_or_else(|| {
            StoreError::Invariant(format!("no stored block at the fork height {fork_height}"))
        })?)
    } else {
        None
    };
    let detections_removed = store.rollback_to(fork_height)?;
    // Stored blocks are contiguous, so this is how many were above the fork.
    let blocks_rolled_back = u64::from(checkpoint.height - fork_height);
    Ok(Some((
        ReorgReport {
            abandoned_checkpoint: checkpoint,
            fork_height,
            blocks_rolled_back,
            detections_removed,
        },
        new_checkpoint,
    )))
}

/// Brings `store` up to the source's tip (or to the limits in `options`).
pub fn sync(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    store: &mut dyn SyncStore,
    options: &SyncOptions,
) -> Result<SyncReport, SyncError> {
    sync_with_progress(source, scanner, wallet_id, store, options, &mut |_| {})
}

/// Like [`sync`], calling `on_commit` with the new checkpoint after every
/// committed chunk (for progress display).
///
/// Error contract: if the call fails *before* changing the store, the plain
/// error is returned. If it fails *after* changing it — a reorg rollback or
/// committed chunks — the error is [`SyncError::Interrupted`], carrying a
/// report of exactly what was changed, so that no rollback or detection is
/// ever applied without being reported.
pub fn sync_with_progress(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    store: &mut dyn SyncStore,
    options: &SyncOptions,
    on_commit: &mut dyn FnMut(BlockRef),
) -> Result<SyncReport, SyncError> {
    if options.chunk_size == 0 {
        return Err(SyncError::InvalidChunkSize);
    }
    let meta = store.meta()?.ok_or(StoreError::NotInitialized)?;
    check_binding(&meta, source, scanner, wallet_id)?;
    let backend_id = source.backend_id().to_string();

    let source_tip = source.tip()?;
    // Everything that can be refused without touching the store comes first.
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
    let resumed_from = store.checkpoint()?;
    // From here on the checkpoint is tracked in memory: after the store has
    // been changed, nothing below depends on reading it back.
    let (reorg, checkpoint) = match resumed_from {
        Some(stored) => match resolve_reorg(
            source,
            store,
            &meta,
            stored,
            source_tip,
            options.max_reorg_depth,
        )? {
            Some((reorg, after)) => (Some(reorg), after),
            None => (None, Some(stored)),
        },
        None => (None, None),
    };

    let mut report = SyncReport {
        chain: meta.chain,
        backend_id: backend_id.clone(),
        evidence_kind: source.capabilities().evidence_kind,
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
        checkpoint,
        source_tip,
        caught_up: false,
        stopped_because: StopReason::CaughtUp,
    };

    let outcome = advance(
        source,
        scanner,
        store,
        options,
        &meta,
        &backend_id,
        target,
        &mut report,
        on_commit,
    );
    match outcome {
        Ok(reason) => {
            report.caught_up = report
                .checkpoint
                .is_some_and(|cp| cp.height == source_tip.height && cp.hash == source_tip.hash);
            report.stopped_because = if report.caught_up {
                StopReason::CaughtUp
            } else if reason == StopReason::CaughtUp {
                // Reached the height of the sampled tip, but on another block.
                StopReason::TipChanged
            } else {
                reason
            };
            Ok(report)
        }
        Err(cause) => {
            let changed = report.reorg.is_some() || report.blocks_scanned > 0;
            if !changed {
                return Err(cause);
            }
            report.stopped_because = StopReason::Error;
            Err(SyncError::Interrupted {
                cause: Box::new(cause),
                partial: Box::new(report),
            })
        }
    }
}

/// The fetch / scan / commit loop. Returns why it stopped.
#[allow(clippy::too_many_arguments)]
fn advance(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    store: &mut dyn SyncStore,
    options: &SyncOptions,
    meta: &StoreMeta,
    backend_id: &str,
    target: u32,
    report: &mut SyncReport,
    on_commit: &mut dyn FnMut(BlockRef),
) -> Result<StopReason, SyncError> {
    if meta.birthday > report.source_tip.height {
        return Ok(StopReason::BirthdayAboveTip);
    }
    loop {
        let checkpoint = report.checkpoint;
        let next = match checkpoint {
            Some(cp) => match cp.height.checked_add(1) {
                Some(next) => next,
                None => return Ok(StopReason::CaughtUp),
            },
            None => meta.birthday,
        };
        if next > target {
            return Ok(if target < report.source_tip.height {
                StopReason::TargetReached
            } else {
                StopReason::CaughtUp
            });
        }
        if options
            .max_blocks
            .is_some_and(|max| report.blocks_scanned >= max)
        {
            return Ok(StopReason::MaxBlocksReached);
        }
        let last = target.min(next.saturating_add(options.chunk_size - 1));

        let batch = source.fetch_range(next, last)?;
        check_batch(backend_id, &batch.blocks, next, last)?;
        if let (Some(cp), Some(first)) = (checkpoint, batch.blocks.first()) {
            if first.block.prev_hash != cp.hash {
                return Err(SyncError::ChainMovedDuringSync { height: next });
            }
        }

        let mut counters = ScanCounters::default();
        let detections = scan_blocks(
            backend_id,
            meta.chain,
            scanner,
            &batch.blocks,
            &mut counters,
            &mut BTreeSet::new(),
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
        if let Some(last) = blocks.last() {
            report.checkpoint = Some(*last);
            on_commit(*last);
        }
    }
}
