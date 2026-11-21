//! Persistent sync state, as an abstraction.
//!
//! `sync-core` defines *what* must be stored and which invariants every store
//! has to enforce; it does not depend on a database. `lab-storage` implements
//! [`SyncStore`] on SQLite, and [`MemoryStore`] here is the reference
//! implementation used in tests.
//!
//! A store never holds key material: no scan key, no spend key, no output
//! tweaks. It holds public chain data and which outputs were detected, which
//! is privacy-sensitive (it is the wallet's receive history) but not secret
//! key material.

use crate::{
    bitcoin::OutPoint,
    models::{BlockRef, DetectedOutput},
    network::ChainIdentity,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Identifies the scanning logic that produced the stored detections. A store
/// written by a different version must be rescanned rather than extended.
pub const SCANNER_VERSION: &str = concat!(
    "sync-core/",
    env!("CARGO_PKG_VERSION"),
    "+bdk_sp@bb373e566fa2"
);

/// What a store is bound to. Every sync verifies these against the wallet and
/// source it is given; any difference is an error, never a silent mix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreMeta {
    pub chain: ChainIdentity,
    /// Public identifier of the receiver (derived from its public keys).
    pub wallet_id: String,
    pub backend_id: String,
    /// Label integers scanned for, ascending. Changing them requires a rescan:
    /// blocks already scanned were not checked for the new labels.
    pub labels: Vec<u32>,
    /// First block height that is scanned. Payments confirmed below it are
    /// never found.
    pub birthday: u32,
    /// Why `birthday` is believed to be at or below the wallet's first
    /// payment. Recorded because a wrong birthday silently loses payments.
    pub birthday_basis: String,
    pub scanner_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    #[error("the store has not been initialized")]
    NotInitialized,
    #[error("the store is already initialized")]
    AlreadyInitialized,
    #[error("store invariant violated: {0}")]
    Invariant(String),
    #[error("storage backend error: {0}")]
    Backend(String),
}

/// Persistent state of one wallet on one chain through one backend.
///
/// Implementations must make [`append`](SyncStore::append) and
/// [`rollback_to`](SyncStore::rollback_to) atomic: after a crash at any point
/// the store holds either the state before the call or the state after it.
pub trait SyncStore {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError>;
    /// Binds an empty store to `meta`. Fails if it is already initialized.
    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError>;
    /// The highest block whose scan result is stored, if any.
    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError>;
    /// The stored block at `height`, used to locate a fork point.
    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError>;
    /// Atomically records that `blocks` were scanned and that `detections`
    /// were found in them. Must enforce [`check_append`].
    fn append(
        &mut self,
        blocks: &[BlockRef],
        detections: &[DetectedOutput],
    ) -> Result<(), StoreError>;
    /// Atomically forgets every block above `height` and the detections in
    /// them; returns the removed detections, ascending.
    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError>;
    /// All stored detections, ordered by (height, txid, vout).
    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError>;
    /// Number of stored (scanned) blocks.
    fn block_count(&self) -> Result<u64, StoreError>;
}

/// The rules every [`SyncStore::append`] must enforce, given the current
/// state. Shared so that all implementations reject exactly the same input.
///
/// * the store is initialized;
/// * `blocks` is non-empty, ascending by one, each linked to the previous;
/// * the first block extends the checkpoint (height + 1 and `prev_hash` equal
///   to the checkpoint's hash), or sits at the birthday when nothing is stored;
/// * every detection belongs to one of `blocks` (same height and hash);
/// * no output is recorded twice, neither within the call nor against
///   `already_stored`.
pub fn check_append(
    meta: Option<&StoreMeta>,
    checkpoint: Option<BlockRef>,
    blocks: &[BlockRef],
    detections: &[DetectedOutput],
    already_stored: &dyn Fn(OutPoint) -> bool,
) -> Result<(), StoreError> {
    let meta = meta.ok_or(StoreError::NotInitialized)?;
    let invariant = |msg: String| StoreError::Invariant(msg);
    let first = blocks
        .first()
        .ok_or_else(|| invariant("append of zero blocks".to_string()))?;

    match checkpoint {
        Some(cp) => {
            if u64::from(first.height) != u64::from(cp.height) + 1 || first.prev_hash != cp.hash {
                return Err(invariant(format!(
                    "block {} ({}) does not extend the checkpoint {} ({})",
                    first.height, first.hash, cp.height, cp.hash
                )));
            }
        }
        None => {
            if first.height != meta.birthday {
                return Err(invariant(format!(
                    "first stored block must be the birthday {} but is {}",
                    meta.birthday, first.height
                )));
            }
        }
    }
    for pair in blocks.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if u64::from(b.height) != u64::from(a.height) + 1 || b.prev_hash != a.hash {
            return Err(invariant(format!(
                "block {} does not follow block {}",
                b.height, a.height
            )));
        }
    }

    let mut seen = BTreeSet::new();
    for d in detections {
        if d.network != meta.chain.network() {
            return Err(invariant(format!(
                "detection on {} in a {} store",
                d.network,
                meta.chain.network()
            )));
        }
        if !blocks.contains(&d.block) {
            return Err(invariant(format!(
                "detection {}:{} refers to block {} ({}) which is not being appended",
                d.txid, d.vout, d.block.height, d.block.hash
            )));
        }
        let outpoint = OutPoint::new(d.txid, d.vout);
        if !seen.insert(outpoint) || already_stored(outpoint) {
            return Err(invariant(format!("output {outpoint} recorded twice")));
        }
    }
    Ok(())
}

/// Canonical order of detections in every store.
pub fn sort_detections(detections: &mut [DetectedOutput]) {
    detections.sort_by_key(|d| (d.block.height, d.txid, d.vout));
}

/// In-memory [`SyncStore`]. Reference implementation; also lets tests inject
/// failures through [`MemoryStore::fail_appends_after`].
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    meta: Option<StoreMeta>,
    blocks: Vec<BlockRef>,
    detections: Vec<DetectedOutput>,
    /// When `Some(n)`, the `n + 1`-th and later appends fail without changing
    /// anything (simulates a crash before commit).
    appends_before_failure: Option<u32>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// The next `n` appends succeed; every later one fails atomically.
    pub fn fail_appends_after(&mut self, n: u32) {
        self.appends_before_failure = Some(n);
    }

    /// Lets appends succeed again.
    pub fn stop_failing(&mut self) {
        self.appends_before_failure = None;
    }
}

impl SyncStore for MemoryStore {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        Ok(self.meta.clone())
    }

    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError> {
        if self.meta.is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        if meta.birthday == 0 {
            return Err(StoreError::Invariant(
                "birthday must be at least 1".to_string(),
            ));
        }
        self.meta = Some(meta.clone());
        Ok(())
    }

    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError> {
        Ok(self.blocks.last().copied())
    }

    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError> {
        Ok(self.blocks.iter().find(|b| b.height == height).copied())
    }

    fn append(
        &mut self,
        blocks: &[BlockRef],
        detections: &[DetectedOutput],
    ) -> Result<(), StoreError> {
        check_append(
            self.meta.as_ref(),
            self.blocks.last().copied(),
            blocks,
            detections,
            &|outpoint| {
                self.detections
                    .iter()
                    .any(|d| OutPoint::new(d.txid, d.vout) == outpoint)
            },
        )?;
        if let Some(remaining) = self.appends_before_failure.as_mut() {
            if *remaining == 0 {
                return Err(StoreError::Backend("injected failure".to_string()));
            }
            *remaining -= 1;
        }
        self.blocks.extend_from_slice(blocks);
        self.detections.extend_from_slice(detections);
        sort_detections(&mut self.detections);
        Ok(())
    }

    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        if self.meta.is_none() {
            return Err(StoreError::NotInitialized);
        }
        self.blocks.retain(|b| b.height <= height);
        let (kept, mut removed): (Vec<_>, Vec<_>) = std::mem::take(&mut self.detections)
            .into_iter()
            .partition(|d| d.block.height <= height);
        self.detections = kept;
        sort_detections(&mut removed);
        Ok(removed)
    }

    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError> {
        Ok(self.detections.clone())
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        Ok(self.blocks.len() as u64)
    }
}
