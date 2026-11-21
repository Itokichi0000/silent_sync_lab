//! Persistent sync state, as an abstraction.
//!
//! `silent-sync` defines *what* must be stored and which invariants every store
//! has to enforce; it does not depend on a database. `silent-sync-sqlite` implements
//! [`SyncStore`] on SQLite, and [`MemoryStore`] here is the reference
//! implementation used in tests.
//!
//! A store never holds key material: no scan key, no spend key, no output
//! tweaks. It holds public chain data and which outputs were detected, which
//! is privacy-sensitive (it is the wallet's receive history) but not secret
//! key material.

use crate::{
    bitcoin::{BlockHash, OutPoint},
    models::{BlockRef, DetectedOutput},
    network::ChainIdentity,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Identifies the scanning logic that produced the stored detections. A store
/// written by a different version must be rescanned rather than extended.
///
/// The `sync-core/` prefix is this crate's former name. It is an identifier,
/// not a crate name, and is kept so that stores written before the rename
/// remain valid.
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
#[non_exhaustive]
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
/// * no block is its own parent, and no block hash occurs twice — within the
///   call or against `block_hash_stored`;
/// * every detection belongs to one of `blocks` (same height *and* hash), is
///   on the store's network, came from the store's backend and has a value
///   every store can represent;
/// * no output is recorded twice, neither within the call nor against
///   `already_stored`.
pub fn check_append(
    meta: Option<&StoreMeta>,
    checkpoint: Option<BlockRef>,
    blocks: &[BlockRef],
    detections: &[DetectedOutput],
    already_stored: &dyn Fn(OutPoint) -> bool,
    block_hash_stored: &dyn Fn(BlockHash) -> bool,
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

    let mut hashes = BTreeSet::new();
    for b in blocks {
        if b.hash == b.prev_hash {
            return Err(invariant(format!(
                "block {} claims to be its own parent",
                b.height
            )));
        }
        if !hashes.insert(b.hash) || block_hash_stored(b.hash) {
            return Err(invariant(format!(
                "block hash {} (height {}) is already recorded",
                b.hash, b.height
            )));
        }
    }

    let mut seen = BTreeSet::new();
    for d in detections {
        if d.value_sats > MAX_STORABLE_SATS {
            return Err(invariant(format!(
                "value of {}:{} does not fit the store",
                d.txid, d.vout
            )));
        }
        if d.provenance.backend_id != meta.backend_id {
            return Err(invariant(format!(
                "detection from backend `{}` in a store bound to `{}`",
                d.provenance.backend_id, meta.backend_id
            )));
        }
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

/// Largest output value a store accepts (what a signed 64-bit column holds;
/// far above the 21 million coin supply).
pub const MAX_STORABLE_SATS: u64 = i64::MAX as u64;

/// Canonical order of detections in every store: by height, then by `Txid`'s
/// own ordering (its internal byte order — *not* the order of the displayed
/// hex string), then by output index. Every implementation must return
/// detections in exactly this order.
pub fn sort_detections(detections: &mut [DetectedOutput]) {
    detections.sort_by_key(|d| (d.block.height, d.txid, d.vout));
}

/// In-memory [`SyncStore`]: the reference implementation of the trait's
/// contract. Nothing is persisted; use it for tests and short-lived scans.
#[derive(Debug, Clone, Default)]
pub struct MemoryStore {
    meta: Option<StoreMeta>,
    blocks: Vec<BlockRef>,
    detections: Vec<DetectedOutput>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
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
            &|hash| self.blocks.iter().any(|b| b.hash == hash),
        )?;
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
        if self.meta.is_none() {
            return Err(StoreError::NotInitialized);
        }
        Ok(self.detections.clone())
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        Ok(self.blocks.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::SCANNER_VERSION;

    /// The scanner version names the upstream revision the workspace actually
    /// builds against: bumping the `bdk_sp` pin without bumping the version
    /// (which forces stores to be rescanned) fails here.
    #[test]
    fn scanner_version_names_the_pinned_upstream_revision() {
        let manifest = include_str!("../../../Cargo.toml");
        let pinned: Vec<&str> = manifest
            .lines()
            .filter(|line| line.trim_start().starts_with("bdk_sp"))
            .filter_map(|line| line.split("rev = \"").nth(1))
            .filter_map(|rest| rest.split('"').next())
            .collect();
        let [rev] = pinned[..] else {
            panic!("expected exactly one pinned bdk_sp revision, found {pinned:?}");
        };
        assert_eq!(rev.len(), 40, "the pin must be a full commit id");
        let (version, upstream) = SCANNER_VERSION
            .split_once("+bdk_sp@")
            .expect("version has an upstream part");
        assert_eq!(version, concat!("sync-core/", env!("CARGO_PKG_VERSION")));
        assert_eq!(upstream.len(), 12);
        assert!(
            rev.starts_with(upstream),
            "SCANNER_VERSION says bdk_sp@{upstream}, the workspace pins {rev}"
        );
    }
}
