//! Implement your own `SyncStore`.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! A store records which blocks were scanned and what was detected in them.
//! The contract is small and the crate provides the two functions that carry
//! it: `check_append` (what may be appended) and `sort_detections` (the order
//! in which detections are returned). This example keeps everything in
//! vectors; a real one would write to a database inside one transaction per
//! `append` / `rollback_to`.
//!
//! ```sh
//! cargo run -p silent-sync --example custom_store --features fixture \
//!     -- fixtures/local/regtest_basic_v1.json
//! ```

use silent_sync::{
    bitcoin::{
        secp256k1::{PublicKey, SecretKey},
        OutPoint,
    },
    new_store_meta,
    source::fixture::FixtureSource,
    store::{check_append, sort_detections, sort_spends},
    sync, BlockRef, DetectedOutput, ReceiveScanner, ReceiverKeys, SpentOutput, StoreError,
    StoreMeta, SyncOptions, SyncStore,
};
use std::{error::Error, path::Path};

#[derive(Default)]
struct VecStore {
    meta: Option<StoreMeta>,
    blocks: Vec<BlockRef>,
    detections: Vec<DetectedOutput>,
    spends: Vec<SpentOutput>,
}

impl SyncStore for VecStore {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        Ok(self.meta.clone())
    }

    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError> {
        if self.meta.is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        if meta.birthday == 0 {
            return Err(StoreError::Invariant("birthday must be at least 1".into()));
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
        spends: &[SpentOutput],
    ) -> Result<(), StoreError> {
        // The shared rules: linked to the checkpoint, nothing recorded twice,
        // every detection in one of the appended blocks, every spend of a
        // detected output, ...
        check_append(
            self.meta.as_ref(),
            self.blocks.last().copied(),
            blocks,
            detections,
            spends,
            &|outpoint| {
                self.detections
                    .iter()
                    .find(|d| OutPoint::new(d.txid, d.vout) == outpoint)
                    .map(|d| d.block.height)
            },
            &|outpoint| {
                self.spends
                    .iter()
                    .any(|s| OutPoint::new(s.txid, s.vout) == outpoint)
            },
            &|hash| self.blocks.iter().any(|b| b.hash == hash),
        )?;
        // All or nothing: a real store commits everything in one transaction.
        self.blocks.extend_from_slice(blocks);
        self.detections.extend_from_slice(detections);
        sort_detections(&mut self.detections);
        self.spends.extend_from_slice(spends);
        sort_spends(&mut self.spends);
        Ok(())
    }

    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        if self.meta.is_none() {
            return Err(StoreError::NotInitialized);
        }
        self.blocks.retain(|b| b.height <= height);
        // An output spent in a forgotten block is unspent again.
        self.spends.retain(|s| s.spent_in.height <= height);
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

    fn spends(&self) -> Result<Vec<SpentOutput>, StoreError> {
        if self.meta.is_none() {
            return Err(StoreError::NotInitialized);
        }
        Ok(self.spends.clone())
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        Ok(self.blocks.len() as u64)
    }
}

/// The receiver of BIP352 test vector #0: a published test key that the
/// fixture's payments are addressed to. Never use it for anything of value.
fn test_vector_keys() -> Result<ReceiverKeys, Box<dyn Error>> {
    let scan: SecretKey =
        "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c".parse()?;
    let spend: PublicKey =
        "025cc9856d6f8375350e123978daac200c260cb5b5ae83106cab90484dcd8fcf36".parse()?;
    Ok(ReceiverKeys::new(scan, spend))
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: custom_store <fixture.json>")?;
    let keys = test_vector_keys()?;
    let scanner = ReceiveScanner::new(&keys, &[1])?;
    let source = FixtureSource::from_path(Path::new(&path))?;

    let mut store = VecStore::default();
    store.initialize(&new_store_meta(
        &source,
        &scanner,
        1,
        "the fixture chain starts at height 1",
    ))?;

    // Three blocks at a time; calling `sync` again later continues from the
    // checkpoint.
    let options = SyncOptions::default().with_chunk_size(3);
    let report = sync(&source, &scanner, &mut store, &options)?;
    println!(
        "scanned {} blocks, caught up: {}",
        report.blocks_scanned, report.caught_up
    );
    for d in store.detections()? {
        println!(
            "  height {} {}:{} {} sat, label {:?}, evidence {:?}",
            d.block.height, d.txid, d.vout, d.value_sats, d.label, d.provenance.evidence_kind
        );
    }
    println!("detected outputs are not a balance: spent status is not tracked");
    Ok(())
}
