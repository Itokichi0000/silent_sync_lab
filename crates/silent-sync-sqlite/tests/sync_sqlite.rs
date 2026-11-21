//! Gate 3 — the persistent sync against SQLite, plus what only a real
//! database file can show: persistence across reopen, refusal to overwrite,
//! schema checks, file permissions and atomic rollback of a failed commit.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{fixture_gen, sync_store_suite, sync_suite::block_refs, test_source::TestSource};
use silent_sync::{
    protocol::ReceiveScanner,
    source::ScanSource,
    store::{StoreError, SyncStore},
    sync::{new_store_meta, sync, SyncOptions},
};
use silent_sync_sqlite::SqliteStore;
use std::path::PathBuf;

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
    let half = SyncOptions::default()
        .with_chunk_size(2)
        .with_max_blocks(Some(4))
        .with_target_height(None);
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

/// A store on disk, bound to the fixture chain and synced to its tip.
fn synced_store(db: &TempDb) -> SqliteStore {
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
    sync(
        &source,
        &scanner(),
        &wallet_id(),
        &mut store,
        &SyncOptions::default(),
    )
    .unwrap();
    store
}

/// Runs SQL on the file through a second, raw connection.
fn raw(db: &TempDb, sql: &str) {
    rusqlite::Connection::open(&db.path)
        .unwrap()
        .execute_batch(sql)
        .unwrap();
}

/// A commit that fails after some rows were written leaves nothing behind:
/// the block rows are inserted first, then the first detection insert aborts.
#[test]
fn failed_append_is_rolled_back_entirely() {
    let db = TempDb::new("atomic-append");
    let full = synced_store(&db);
    let detections = full.detections().unwrap();
    drop(full);
    std::fs::remove_file(&db.path).unwrap();

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
    let in_blocks: Vec<_> = detections
        .iter()
        .filter(|d| (2..=3).contains(&d.block.height))
        .cloned()
        .collect();
    assert!(
        in_blocks.first().is_some_and(|d| d.block.height == 2)
            && in_blocks.last().is_some_and(|d| d.block.height == 3),
        "a detection at height 2 is inserted before the one that fails"
    );

    raw(
        &db,
        "CREATE TRIGGER injected_failure BEFORE INSERT ON detected_outputs
         WHEN NEW.height = 3 BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    );
    // Blocks 2 and 3 and the detection at height 2 are written, then the
    // detection at height 3 aborts.
    assert!(matches!(
        store.append(&blocks[1..], &in_blocks),
        Err(StoreError::Backend(m)) if m.contains("injected failure")
    ));
    assert_eq!(store.block_count().unwrap(), 1);
    assert!(store.detections().unwrap().is_empty());
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(1));
    drop(store);

    // Also on disk, and the same append works once the fault is gone.
    let mut store = SqliteStore::open(&db.path).unwrap();
    assert_eq!(store.block_count().unwrap(), 1);
    assert!(store.detections().unwrap().is_empty());
    raw(&db, "DROP TRIGGER injected_failure;");
    store.append(&blocks[1..], &in_blocks).unwrap();
    assert_eq!(store.detections().unwrap(), in_blocks);
}

/// A rollback that fails half-way (detections already deleted, blocks not)
/// leaves the store exactly as it was.
#[test]
fn failed_rollback_removes_nothing() {
    let db = TempDb::new("atomic-rollback");
    let mut store = synced_store(&db);
    let before = (
        store.checkpoint().unwrap(),
        store.block_count().unwrap(),
        store.detections().unwrap(),
    );
    assert_eq!(before.2.len(), 7);

    raw(
        &db,
        "CREATE TRIGGER injected_failure BEFORE DELETE ON blocks
         BEGIN SELECT RAISE(ABORT, 'injected failure'); END;",
    );
    assert!(matches!(
        store.rollback_to(5),
        Err(StoreError::Backend(m)) if m.contains("injected failure")
    ));
    let now = (
        store.checkpoint().unwrap(),
        store.block_count().unwrap(),
        store.detections().unwrap(),
    );
    assert_eq!(now, before, "a failed rollback must not remove detections");
    drop(store);

    let mut store = SqliteStore::open(&db.path).unwrap();
    assert_eq!(store.detections().unwrap(), before.2);
    raw(&db, "DROP TRIGGER injected_failure;");
    let removed = store.rollback_to(5).unwrap();
    assert_eq!(removed, before.2[5..]);
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(5));
}

/// A file that was edited or damaged is refused when it is opened, instead of
/// being synced on top of.
#[test]
fn damaged_stores_are_refused_on_open() {
    let refused = |name: &str, damage: &str, needle: &str| {
        let db = TempDb::new(name);
        drop(synced_store(&db));
        raw(&db, damage);
        match SqliteStore::open(&db.path) {
            Err(StoreError::Invariant(m)) => {
                assert!(
                    m.contains(needle),
                    "{name}: `{m}` should mention `{needle}`"
                )
            }
            Err(other) => panic!("{name}: expected an invariant violation, got {other:?}"),
            Ok(_) => panic!("{name}: a damaged store was opened"),
        }
    };
    // Rows are removed children first (foreign keys are enforced by default).
    refused(
        "gap",
        "DELETE FROM detected_outputs WHERE height = 4; DELETE FROM blocks WHERE height = 4;",
        "not contiguous from the birthday",
    );
    refused(
        "head",
        "DELETE FROM detected_outputs WHERE height = 1; DELETE FROM blocks WHERE height = 1;",
        "not contiguous from the birthday",
    );
    refused(
        "link",
        "UPDATE blocks SET prev_hash = (SELECT hash FROM blocks WHERE height = 1) WHERE height = 6;",
        "not hash-linked",
    );
    refused(
        "self-parent",
        "UPDATE blocks SET prev_hash = hash WHERE height = 1;",
        "not hash-linked",
    );
    refused(
        "binding-garbage",
        "UPDATE meta SET value = '{\"not\": \"a binding\"}' WHERE key = 'store_meta';",
        "stored binding is not valid",
    );
    refused(
        "binding-mainnet",
        "UPDATE meta SET value = replace(value, '\"regtest\"', '\"mainnet\"') WHERE key = 'store_meta';",
        "stored binding is not valid",
    );
    refused(
        "birthday-zero",
        "UPDATE meta SET value = replace(value, '\"birthday\":1', '\"birthday\":0') WHERE key = 'store_meta';",
        "stored binding (birthday 0) is not valid",
    );
    refused(
        "orphan-detection",
        "PRAGMA foreign_keys = OFF; \
         INSERT INTO detected_outputs (txid, vout, height, value_sats, label, backend_id, evidence_kind) \
         SELECT txid, 7, 9999, value_sats, label, backend_id, evidence_kind FROM detected_outputs LIMIT 1;",
        "refer to blocks that are not stored",
    );
    refused(
        "foreign-backend",
        "UPDATE detected_outputs SET backend_id = 'somebody_else' WHERE height = 2;",
        "from another backend than `fixture`",
    );
    refused(
        "no-binding",
        "DELETE FROM meta WHERE key = 'store_meta';",
        "without a store binding",
    );

    // The same checks let an undamaged store through, at any length.
    let db = TempDb::new("intact");
    let mut store = synced_store(&db);
    store.rollback_to(0).unwrap();
    drop(store);
    assert_eq!(
        SqliteStore::open(&db.path).unwrap().block_count().unwrap(),
        0
    );
}

/// Rows that cannot be a value this code wrote are an error when read, never
/// a silently different value.
#[test]
fn rows_that_cannot_be_valid_are_errors_when_read() {
    let unreadable = |name: &str, damage: &str, needle: &str| {
        let db = TempDb::new(name);
        drop(synced_store(&db));
        raw(&db, damage);
        let store = SqliteStore::open(&db.path).unwrap();
        match store.detections() {
            Err(StoreError::Invariant(m)) => {
                assert!(
                    m.contains(needle),
                    "{name}: `{m}` should mention `{needle}`"
                )
            }
            other => panic!("{name}: expected an invariant violation, got {other:?}"),
        }
    };
    unreadable(
        "row-vout",
        "UPDATE detected_outputs SET vout = -1 WHERE height = 2;",
        "stored vout is not valid",
    );
    unreadable(
        "row-vout-large",
        "UPDATE detected_outputs SET vout = 4294967296 WHERE height = 2;",
        "stored vout is not valid",
    );
    unreadable(
        "row-value",
        "UPDATE detected_outputs SET value_sats = -5 WHERE height = 2;",
        "stored value is not valid",
    );
    unreadable(
        "row-label",
        "UPDATE detected_outputs SET label = -1 WHERE height = 2;",
        "stored label is not valid",
    );
    unreadable(
        "row-txid",
        "UPDATE detected_outputs SET txid = 'zz' WHERE height = 2;",
        "stored txid is not valid",
    );
    unreadable(
        "row-kind",
        "UPDATE detected_outputs SET evidence_kind = 'hearsay' WHERE height = 2;",
        "stored evidence kind is not valid",
    );

    // A block hash that is not a hash: the checkpoint cannot be read.
    let db = TempDb::new("row-hash");
    drop(synced_store(&db));
    raw(
        &db,
        "UPDATE blocks SET hash = 'not-a-hash' WHERE height = 8;",
    );
    let store = SqliteStore::open(&db.path).unwrap();
    assert!(matches!(
        store.checkpoint(),
        Err(StoreError::Invariant(m)) if m.contains("stored block hash is not valid")
    ));
}

/// A second process holding the write lock for a moment delays a commit; it
/// does not make it fail, and it cannot interleave with it.
#[test]
fn a_briefly_locked_file_delays_the_commit() {
    let db = TempDb::new("busy");
    let mut store = synced_store(&db);
    store.rollback_to(6).unwrap();
    let blocks = block_refs(7, 8);

    let other = rusqlite::Connection::open(&db.path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE;").unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        other.execute_batch("COMMIT;").unwrap();
    });
    let started = std::time::Instant::now();
    store.append(&blocks, &[]).unwrap();
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(300),
        "the commit must have waited for the lock"
    );
    holder.join().unwrap();
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(8));
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
        let silent_sync::source::Evidence::FullBlock(txs) = &batch.blocks[0].evidence else {
            panic!("fixture yields full blocks");
        };
        for entry in txs {
            if let Ok(silent_sync::protocol::TxScanOutcome::Scanned { matches }) =
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
