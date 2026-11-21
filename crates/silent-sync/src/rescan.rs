//! Scanning a store's range again into a new store.
//!
//! A store is bound to the label set it was scanned with: blocks already
//! scanned were not checked for other labels, and an output sent to an
//! unknown label also hides the outputs after it in the same transaction.
//! Finding payments to a label that was added later therefore means scanning
//! everything again — there is no shortcut that does not involve keeping
//! per-transaction secrets around.
//!
//! [`rescan_into`] does that without ever touching the store in use: it fills
//! a *fresh* store with the result of scanning from the same birthday with
//! the scanner it is given, and returns `Ok` only when the fresh store can
//! take the place of the current one — it reaches at least as far, on the
//! same chain, and holds everything the current store holds. Replacing the
//! one with the other is left to the caller (for the SQLite store, an atomic
//! file rename), so a failure at any point leaves the current store exactly
//! as it was.
//!
//! The same function rebuilds a store written by another scanner version.
//!
//! A rescan that was interrupted is continued by calling the function again
//! with the same fresh store. What that store already holds is then taken as
//! it is — it is not scanned a second time — so a fresh store must be kept as
//! carefully as the store in use between the calls.
//! [`RescanReport::continued_from`] says how much came from earlier calls.

use crate::{
    bitcoin::OutPoint,
    models::{BlockRef, DetectedOutput},
    protocol::ReceiveScanner,
    source::ScanSource,
    store::{bound_meta, StoreError, StoreMeta, SyncStore, SCANNER_VERSION},
    sync::{binding_difference, sync, SyncError, SyncOptions, SyncReport},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Why a rescan did not produce a store that can replace the current one.
/// In every case the current store is unchanged. Unless stated otherwise the
/// fresh store keeps what was scanned so far, and calling [`rescan_into`]
/// again with the same arguments continues from there.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RescanError {
    #[error("the store in use: {0}")]
    /// The store in use failed, or is not a usable store.
    CurrentStore(StoreError),
    #[error("the fresh store: {0}")]
    /// The fresh store failed or refused an operation.
    FreshStore(StoreError),
    #[error(transparent)]
    /// Scanning into the fresh store failed or was interrupted.
    Sync(#[from] SyncError),
    #[error("the store is bound to a different {field}: stored `{stored}`, given `{given}`")]
    /// The current store is for another chain, receiver or backend than the
    /// call was given. Nothing was written to the fresh store.
    BindingMismatch {
        /// Which part of the binding differs.
        field: &'static str,
        /// What the current store is bound to.
        stored: String,
        /// What the call was given.
        given: String,
    },
    #[error(
        "the scanner does not know every label the store was scanned with (store {stored:?}, \
         scanner {given:?}); outputs found through a missing label would be lost"
    )]
    /// The scanner lacks a label of the current store. Labels can be added,
    /// not removed. Nothing was written to the fresh store.
    LabelsRemoved {
        /// Label set of the current store.
        stored: Vec<u32>,
        /// Label set of the scanner.
        given: Vec<u32>,
    },
    #[error(
        "the fresh store is already bound to something else than this rescan would bind it to; \
         use an empty store"
    )]
    /// The fresh store was initialized for something else (another label
    /// set, receiver, birthday, ...). It was not changed.
    FreshStoreInUse,
    #[error(
        "the rescan has reached {} but the store in use is at height {needed}; \
         run it again to continue",
        .reached.map_or_else(|| "no block yet".to_string(), |h| format!("height {h}"))
    )]
    /// The fresh store does not reach the current store's checkpoint yet:
    /// the call was limited (`max_blocks`, `target_height`) or the source is
    /// behind.
    Incomplete {
        /// Height the fresh store has reached, if any.
        reached: Option<u32>,
        /// Height of the current store's checkpoint.
        needed: u32,
    },
    #[error(
        "the checkpoint of the store in use (height {height}) is not on the chain that was \
         rescanned; sync the store in use first, then run the rescan again"
    )]
    /// The source's chain no longer contains the current store's checkpoint
    /// (a reorganization the current store has not followed yet). Once the
    /// current store has been synced, the same fresh store can be continued.
    CheckpointNotOnChain {
        /// Height of the current store's checkpoint.
        height: u32,
    },
    #[error(
        "the rescan did not reproduce {} output(s) the store in use holds; the two results \
         disagree and the store was not replaced",
        outputs.len()
    )]
    /// The current store holds detections the rescan did not find again, or
    /// found with another amount, label or block. Possible causes:
    ///
    /// * the source answers differently than it did when the current store
    ///   was filled, or one of the two stores was altered;
    /// * a transaction that was not built as BIP352 prescribes: one that
    ///   pays two of the receiver's addresses with the same output counter
    ///   `k`. The upstream scanner reports one output per `k`, so which of
    ///   the two is detected depends on the label set — knowing one more
    ///   label can replace a detected output by another. Anyone who knows
    ///   two of the receiver's addresses can make such a transaction.
    ///
    /// In both cases the two results disagree about what was received, and
    /// the function does not pick one.
    NotReproduced {
        /// The detections of the current store that are not in the fresh
        /// store exactly as they are.
        outputs: Vec<DetectedOutput>,
    },
}

/// Outcome of a successful [`rescan_into`]: the fresh store can take the
/// place of the current one. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct RescanReport {
    /// Label set of the current store.
    pub previous_labels: Vec<u32>,
    /// Label set of the fresh store.
    pub labels: Vec<u32>,
    /// Scanner version that wrote the current store.
    pub previous_scanner_version: String,
    /// Checkpoint of the current store.
    pub previous_checkpoint: Option<BlockRef>,
    /// Checkpoint of the fresh store (at or above the previous one).
    pub checkpoint: Option<BlockRef>,
    /// Detections of the current store, all found again exactly as they are.
    pub reproduced: u64,
    /// Detections the fresh store holds in the range the current store
    /// covers, and the current store does not: what the added labels (or the
    /// newer scanner) revealed.
    pub added: Vec<DetectedOutput>,
    /// Detections in blocks above the current store's checkpoint.
    pub in_new_blocks: u64,
    /// Checkpoint the fresh store already had when this call started: blocks
    /// up to it were scanned by earlier calls and taken as stored. `None`
    /// when this call scanned everything itself.
    pub continued_from: Option<BlockRef>,
    /// Report of the sync that completed the fresh store. When a rescan was
    /// continued, it covers the last call only.
    pub last_sync: SyncReport,
}

/// Scans the range `current` covers again, with `scanner`, into `fresh`.
///
/// * `current` is only read. It must be bound to the same chain, receiver
///   and backend as the call; `scanner` must know every label it was scanned
///   with, and may know more.
/// * `fresh` must be empty (uninitialized), or hold an earlier, unfinished
///   rescan with the same arguments. It is bound like `current`, with the
///   scanner's label set and this version's scanner.
///
/// On `Ok`, `fresh` reaches at least `current`'s checkpoint on the same
/// chain and holds every detection of `current`; the caller may now replace
/// `current` with it. On `Err`, it must not.
pub fn rescan_into(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    current: &dyn SyncStore,
    fresh: &mut dyn SyncStore,
    options: &SyncOptions,
) -> Result<RescanReport, RescanError> {
    let meta = bound_meta(current).map_err(RescanError::CurrentStore)?;
    if let Some(d) = binding_difference(
        &meta,
        source.chain(),
        Some(source.backend_id()),
        scanner,
        false,
    ) {
        return Err(RescanError::BindingMismatch {
            field: d.field,
            stored: d.stored,
            given: d.given,
        });
    }
    let labels: Vec<u32> = scanner.labels().collect();
    if !meta.labels.iter().all(|m| labels.contains(m)) {
        return Err(RescanError::LabelsRemoved {
            stored: meta.labels,
            given: labels,
        });
    }
    let fresh_meta = StoreMeta {
        labels: labels.clone(),
        scanner_version: SCANNER_VERSION.to_string(),
        ..meta.clone()
    };
    let (of_current, of_fresh) = (RescanError::CurrentStore, RescanError::FreshStore);
    match fresh.meta().map_err(of_fresh)? {
        None => fresh.initialize(&fresh_meta).map_err(of_fresh)?,
        Some(existing) if existing == fresh_meta => {}
        Some(_) => return Err(RescanError::FreshStoreInUse),
    }
    let continued_from = fresh.checkpoint().map_err(of_fresh)?;

    let last_sync = sync(source, scanner, fresh, options)?;

    // The fresh store may replace the current one only if it reaches as far,
    // on the same chain ...
    let previous_checkpoint = current.checkpoint().map_err(of_current)?;
    let checkpoint = fresh.checkpoint().map_err(of_fresh)?;
    if let Some(previous) = previous_checkpoint {
        if !checkpoint.is_some_and(|cp| cp.height >= previous.height) {
            return Err(RescanError::Incomplete {
                reached: checkpoint.map(|cp| cp.height),
                needed: previous.height,
            });
        }
        if fresh.block_at(previous.height).map_err(of_fresh)? != Some(previous) {
            return Err(RescanError::CheckpointNotOnChain {
                height: previous.height,
            });
        }
    }

    // ... and holds everything the current one holds, exactly as it is.
    let rescanned = fresh.detections().map_err(of_fresh)?;
    let by_outpoint: BTreeMap<OutPoint, &DetectedOutput> = rescanned
        .iter()
        .map(|d| (OutPoint::new(d.txid, d.vout), d))
        .collect();
    let held = current.detections().map_err(of_current)?;
    let not_reproduced: Vec<DetectedOutput> = held
        .iter()
        .filter(|d| by_outpoint.get(&OutPoint::new(d.txid, d.vout)) != Some(d))
        .cloned()
        .collect();
    if !not_reproduced.is_empty() {
        return Err(RescanError::NotReproduced {
            outputs: not_reproduced,
        });
    }

    let covered = previous_checkpoint.map(|cp| cp.height);
    let held_outpoints: BTreeSet<OutPoint> =
        held.iter().map(|d| OutPoint::new(d.txid, d.vout)).collect();
    let mut added = Vec::new();
    let mut in_new_blocks = 0;
    for d in rescanned {
        if !covered.is_some_and(|height| d.block.height <= height) {
            in_new_blocks += 1;
        } else if !held_outpoints.contains(&OutPoint::new(d.txid, d.vout)) {
            added.push(d);
        }
    }

    Ok(RescanReport {
        previous_labels: meta.labels,
        labels,
        previous_scanner_version: meta.scanner_version,
        previous_checkpoint,
        checkpoint,
        reproduced: held.len() as u64,
        added,
        in_new_blocks,
        continued_from,
        last_sync,
    })
}
