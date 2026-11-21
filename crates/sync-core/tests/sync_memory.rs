//! Gate 3 — the persistent sync against the in-memory reference store.
//! The same suite runs against SQLite in `lab-storage`.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{fixture_gen, sync_store_suite, test_source::TestSource};
use sync_core::{
    protocol::ReceiveScanner,
    source::ScanSource,
    store::{MemoryStore, StoreError, SyncStore},
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
    let options = SyncOptions {
        chunk_size: 3,
        ..SyncOptions::default()
    };
    for good_commits in 0..3u32 {
        let mut store = MemoryStore::new();
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
        assert_eq!(
            result,
            Err(SyncError::Store(StoreError::Backend(
                "injected failure".to_string()
            )))
        );
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
