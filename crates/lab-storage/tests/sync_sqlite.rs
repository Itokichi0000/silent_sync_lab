//! Gate 3 — the persistent sync against SQLite, plus what only a real
//! database file can show: persistence across reopen, refusal to overwrite,
//! schema checks, file permissions and atomic rollback of a failed commit.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_storage::SqliteStore;
use lab_testkit::{fixture_gen, sync_store_suite, sync_suite::block_refs, test_source::TestSource};
use std::path::PathBuf;
use sync_core::{
    protocol::ReceiveScanner,
    source::ScanSource,
    store::{StoreError, SyncStore},
    sync::{new_store_meta, sync, SyncOptions},
};

fn sqlite_store() -> Box<dyn SyncStore> {
    Box::new(SqliteStore::in_memory().unwrap())
}

sync_store_suite!(sqlite_store);

/// A fresh path in a private temporary directory, removed on drop.
struct TempDb {
    dir: PathBuf,
    path: PathBuf,
}

impl TempDb {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ssl-storage-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.sqlite3");
        Self { dir, path }
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(
        &fixture_gen::lab_receiver_keys(),
        &[fixture_gen::LABEL_USED],
    )
    .unwrap()
}

fn wallet_id() -> String {
    fixture_gen::lab_receiver_keys().wallet_id()
}

#[test]
fn state_survives_closing_and_reopening_the_file() {
    let db = TempDb::new("reopen");
    let source = TestSource::new();
    let half = SyncOptions {
        chunk_size: 2,
        max_blocks: Some(4),
        target_height: None,
    };
    {
        let mut store = SqliteStore::create(&db.path).unwrap();
        store
            .initialize(&new_store_meta(
                &source,
                &scanner(),
                &wallet_id(),
                1,
                "fixture genesis",
            ))
            .unwrap();
        let report = sync(&source, &scanner(), &wallet_id(), &mut store, &half).unwrap();
        assert_eq!(report.checkpoint.map(|c| c.height), Some(4));
    } // connection closed here

    let mut store = SqliteStore::open(&db.path).unwrap();
    let meta = store.meta().unwrap().unwrap();
    assert_eq!(
        (meta.birthday, meta.birthday_basis.as_str()),
        (1, "fixture genesis")
    );
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(4));
    assert_eq!(store.detections().unwrap().len(), 3);

    let report = sync(
        &source,
        &scanner(),
        &wallet_id(),
        &mut store,
        &SyncOptions::default(),
    )
    .unwrap();
    assert!(report.caught_up);
    assert_eq!(report.resumed_from.map(|c| c.height), Some(4));
    drop(store);

    let store = SqliteStore::open(&db.path).unwrap();
    assert_eq!(store.checkpoint().unwrap(), Some(source.tip().unwrap()));
    assert_eq!(store.detections().unwrap().len(), 7);
    assert_eq!(store.block_count().unwrap(), 8);
}

#[test]
fn files_are_never_overwritten_and_foreign_files_are_refused() {
    let db = TempDb::new("files");
    assert!(
        matches!(SqliteStore::open(&db.path), Err(StoreError::Backend(m)) if m.contains("does not exist"))
    );

    drop(SqliteStore::create(&db.path).unwrap());
    assert!(
        matches!(SqliteStore::create(&db.path), Err(StoreError::Backend(m)) if m.contains("refusing to overwrite"))
    );
    assert!(SqliteStore::open(&db.path).is_ok());

    // Not a database at all.
    let junk = db.dir.join("junk.sqlite3");
    std::fs::write(&junk, b"this is not a database").unwrap();
    assert!(SqliteStore::open(&junk).is_err());

    // A SQLite file from something else.
    let foreign = db.dir.join("foreign.sqlite3");
    rusqlite::Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE t (x INTEGER);")
        .unwrap();
    assert!(
        matches!(SqliteStore::open(&foreign), Err(StoreError::Backend(m)) if m.contains("not a Silent Sync Lab store"))
    );

    // A store written with another schema version.
    rusqlite::Connection::open(&db.path)
        .unwrap()
        .execute(
            "UPDATE meta SET value = '999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    assert!(
        matches!(SqliteStore::open(&db.path), Err(StoreError::Backend(m)) if m.contains("unsupported store schema version"))
    );
}

#[cfg(unix)]
#[test]
fn database_file_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let db = TempDb::new("mode");
    drop(SqliteStore::create(&db.path).unwrap());
    let mode = std::fs::metadata(&db.path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// A commit that fails after some rows were written leaves nothing behind.
#[test]
fn failed_append_is_rolled_back_entirely() {
    let db = TempDb::new("atomic");
    let source = TestSource::new();
    let mut store = SqliteStore::create(&db.path).unwrap();
    store
        .initialize(&new_store_meta(
            &source,
            &scanner(),
            &wallet_id(),
            1,
            "test",
        ))
        .unwrap();
    let blocks = block_refs(1, 3);
    store.append(&blocks[..1], &[]).unwrap();

    // Two valid blocks plus a detection whose value cannot be stored: the
    // block rows are inserted first, then the detection insert fails.
    let mut reference = SqliteStore::in_memory().unwrap();
    reference
        .initialize(&new_store_meta(
            &source,
            &scanner(),
            &wallet_id(),
            1,
            "test",
        ))
        .unwrap();
    sync(
        &source,
        &scanner(),
        &wallet_id(),
        &mut reference,
        &SyncOptions::default(),
    )
    .unwrap();
    let mut bad = reference
        .detections()
        .unwrap()
        .into_iter()
        .find(|d| d.block.height == 2)
        .unwrap();
    bad.value_sats = u64::MAX;
    assert!(matches!(
        store.append(&blocks[1..], &[bad]),
        Err(StoreError::Invariant(m)) if m.contains("does not fit")
    ));
    assert_eq!(store.block_count().unwrap(), 1);
    assert!(store.detections().unwrap().is_empty());
    drop(store);

    let store = SqliteStore::open(&db.path).unwrap();
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(1));
    assert_eq!(store.block_count().unwrap(), 1);
}

/// The database holds public data only.
#[test]
fn no_key_material_is_written_to_the_database() {
    let db = TempDb::new("secrets");
    let source = TestSource::new();
    let keys = fixture_gen::lab_receiver_keys();
    let (scan_sk, _) =
        fixture_gen::vector_raw_keys(fixture_gen::LAB_RECEIVER.0, fixture_gen::LAB_RECEIVER.1);
    let all_labels = ReceiveScanner::new(
        &keys,
        &[fixture_gen::LABEL_USED, fixture_gen::LABEL_NOT_BACKED_UP],
    )
    .unwrap();
    let mut secrets = vec![scan_sk.display_secret().to_string()];
    for height in 1..=8 {
        let batch = source.fetch_range(height, height).unwrap();
        let sync_core::source::Evidence::FullBlock(txs) = &batch.blocks[0].evidence else {
            panic!("fixture yields full blocks");
        };
        for entry in txs {
            if let Ok(sync_core::protocol::TxScanOutcome::Scanned { matches }) =
                all_labels.scan_tx(&entry.tx, &entry.prevouts)
            {
                secrets.extend(
                    matches
                        .iter()
                        .map(|m| m.sensitive_output_tweak().display_secret().to_string()),
                );
            }
        }
    }
    assert_eq!(secrets.len(), 9);

    {
        let mut store = SqliteStore::create(&db.path).unwrap();
        store
            .initialize(&new_store_meta(
                &source,
                &all_labels,
                &keys.wallet_id(),
                1,
                "test",
            ))
            .unwrap();
        sync(
            &source,
            &all_labels,
            &keys.wallet_id(),
            &mut store,
            &SyncOptions::default(),
        )
        .unwrap();
        assert_eq!(store.detections().unwrap().len(), 8);
    }
    let bytes = std::fs::read(&db.path).unwrap();
    let text = String::from_utf8_lossy(&bytes).to_lowercase();
    for secret in &secrets {
        assert!(
            !text.contains(secret),
            "key material found in the database file"
        );
        let raw: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&secret[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        assert!(
            !bytes.windows(32).any(|w| w == raw.as_slice()),
            "raw key bytes in the database file"
        );
    }
    // Sanity: the public wallet id is there.
    assert!(text.contains(&keys.wallet_id()));
}
