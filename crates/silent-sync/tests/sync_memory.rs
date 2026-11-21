//! Gate 3 — the persistent sync against the in-memory reference store.
//! The same suite runs against SQLite in `silent-sync-sqlite`.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen, sync_store_suite, test_source::TestSource, test_store::FailingAppends,
};
use silent_sync::{
    models::{BlockRef, DetectedOutput},
    protocol::ReceiveScanner,
    source::ScanSource,
    store::{MemoryStore, StoreError, StoreMeta, SyncStore},
    sync::{new_store_meta, sync, SyncError, SyncOptions},
};

fn memory_store() -> Box<dyn SyncStore> {
    Box::new(MemoryStore::new())
}

sync_store_suite!(memory_store);

/// A commit that fails (as a crash before commit would) loses that chunk only:
/// the store is exactly as after the previous chunk, and the next sync redoes
/// the lost chunk.
#[test]
fn failed_commit_loses_only_the_uncommitted_chunk() {
    let source = TestSource::new();
    let keys = fixture_gen::lab_receiver_keys();
    let scanner = ReceiveScanner::new(&keys, &[fixture_gen::LABEL_USED]).unwrap();
    let options = SyncOptions::default().with_chunk_size(3);
    for good_commits in 0..3u32 {
        let mut store = FailingAppends::new(MemoryStore::new());
        store
            .initialize(&new_store_meta(
                &source,
                &scanner,
                &keys.wallet_id(),
                1,
                "test",
            ))
            .unwrap();
        store.fail_appends_after(good_commits);
        let result = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options);
        let injected = SyncError::Store(StoreError::Backend("injected failure".to_string()));
        match result {
            Err(e) if good_commits == 0 => assert_eq!(e, injected),
            Err(SyncError::Interrupted { cause, partial }) => {
                assert_eq!(*cause, injected);
                assert_eq!(partial.blocks_scanned, u64::from(good_commits * 3));
                assert_eq!(partial.checkpoint.map(|c| c.height), Some(good_commits * 3));
                assert_eq!(partial.new_detections, store.detections().unwrap());
            }
            other => panic!("expected the injected failure, got {other:?}"),
        }
        let checkpoint = store.checkpoint().unwrap().map(|c| c.height);
        assert_eq!(checkpoint, (good_commits > 0).then_some(good_commits * 3));
        assert!(store
            .detections()
            .unwrap()
            .iter()
            .all(|d| Some(d.block.height) <= checkpoint));

        store.stop_failing();
        let report = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options).unwrap();
        assert!(report.caught_up);
        assert_eq!(report.resumed_from.map(|c| c.height), checkpoint);
        assert_eq!(store.detections().unwrap().len(), 7);
        assert_eq!(store.checkpoint().unwrap(), Some(source.tip().unwrap()));
    }
}

/// A store whose every read fails once it has been written to in this call
/// sequence: what `sync` reports after a change must not depend on reading
/// the store back.
struct ReadsFailAfterAWrite {
    inner: MemoryStore,
    written: bool,
}

impl ReadsFailAfterAWrite {
    fn read<T>(&self, value: Result<T, StoreError>) -> Result<T, StoreError> {
        if self.written {
            Err(StoreError::Backend("injected read failure".to_string()))
        } else {
            value
        }
    }
}

impl SyncStore for ReadsFailAfterAWrite {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        self.read(self.inner.meta())
    }
    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError> {
        self.inner.initialize(meta)
    }
    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError> {
        self.read(self.inner.checkpoint())
    }
    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError> {
        self.read(self.inner.block_at(height))
    }
    fn append(
        &mut self,
        blocks: &[BlockRef],
        detections: &[DetectedOutput],
    ) -> Result<(), StoreError> {
        self.inner.append(blocks, detections)?;
        self.written = true;
        Ok(())
    }
    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        let removed = self.inner.rollback_to(height)?;
        self.written = true;
        Ok(removed)
    }
    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError> {
        self.read(self.inner.detections())
    }
    fn block_count(&self) -> Result<u64, StoreError> {
        self.read(self.inner.block_count())
    }
}

/// Once the store has been changed, `sync` must be able to report what it
/// did without reading the store again: a failing read after a rollback or a
/// commit must not turn the call into a plain error that hides the change.
#[test]
fn what_was_changed_is_reported_without_reading_the_store_back() {
    let source = TestSource::new();
    let keys = fixture_gen::lab_receiver_keys();
    let scanner = ReceiveScanner::new(&keys, &[fixture_gen::LABEL_USED]).unwrap();
    let synced_on_main = || {
        let mut inner = MemoryStore::new();
        source.on_fork.set(false);
        inner
            .initialize(&new_store_meta(
                &source,
                &scanner,
                &keys.wallet_id(),
                1,
                "test",
            ))
            .unwrap();
        sync(
            &source,
            &scanner,
            &keys.wallet_id(),
            &mut inner,
            &SyncOptions::default(),
        )
        .unwrap();
        ReadsFailAfterAWrite {
            inner,
            written: false,
        }
    };
    let options = SyncOptions::default().with_chunk_size(2);

    // Reorg, then two chunks: the whole call succeeds and reports everything.
    let mut store = synced_on_main();
    source.on_fork.set(true);
    let report = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options).unwrap();
    let reorg = report.reorg.expect("the rollback is reported");
    assert_eq!((reorg.fork_height, reorg.blocks_rolled_back), (5, 3));
    assert_eq!(reorg.detections_removed.len(), 2);
    assert_eq!(report.blocks_scanned, 4);
    assert_eq!(report.checkpoint, Some(source.tip().unwrap()));
    assert!(report.caught_up);
    assert_eq!(
        store.inner.checkpoint().unwrap(),
        report.checkpoint,
        "the reported checkpoint is the stored one"
    );

    // Reorg, one chunk, then the source fails: the partial report names the
    // rollback and the real checkpoint.
    let mut store = synced_on_main();
    source.on_fork.set(true);
    source.fetches_left.set(Some(1));
    let result = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options);
    source.fetches_left.set(None);
    let Err(SyncError::Interrupted { partial, .. }) = result else {
        panic!("expected an interrupted sync, got {result:?}");
    };
    assert_eq!(partial.reorg.as_ref().map(|r| r.fork_height), Some(5));
    assert_eq!(partial.blocks_scanned, 2);
    assert_eq!(partial.checkpoint.map(|c| c.height), Some(7));
    assert_eq!(store.inner.checkpoint().unwrap(), partial.checkpoint);

    // Reorg and an immediate failure: the checkpoint is the fork point.
    let mut store = synced_on_main();
    source.on_fork.set(true);
    source.fetches_left.set(Some(0));
    let result = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options);
    source.fetches_left.set(None);
    let Err(SyncError::Interrupted { partial, .. }) = result else {
        panic!("expected an interrupted sync, got {result:?}");
    };
    assert_eq!(partial.checkpoint.map(|c| c.height), Some(5));
    assert_eq!(store.inner.checkpoint().unwrap(), partial.checkpoint);
    assert_eq!(partial.reorg.map(|r| r.detections_removed.len()), Some(2));
    source.on_fork.set(false);
}
