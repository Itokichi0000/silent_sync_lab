//! SQLite-backed [`SyncStore`].
//!
//! The database holds: the store binding (network, genesis hash, public wallet
//! id, backend, labels, birthday and its basis, scanner version), the hash
//! chain of scanned blocks, and the detected outputs. It never holds key
//! material — no scan key, no spend key, no output tweaks.
//!
//! The detected outputs are the wallet's receive history. The file is created
//! with owner-only permissions on Unix and should not be shared carelessly.
//!
//! Crash safety: `append` and `rollback_to` each run in one SQLite
//! transaction, so a process killed at any point leaves either the previous
//! or the new state on disk.
//!
//! One process per store file is the supported use. Writes take the SQLite
//! write lock up front and wait a few seconds for it, so a second process
//! either waits or fails with a clean error; it cannot interleave a commit.
//!
//! A file is checked when it is opened: a binding that cannot be read, a
//! block chain that is not contiguous and hash-linked from the birthday, or
//! detections without their block or from another backend, are refused
//! rather than synced on top of. This detects damage, not tampering: nothing
//! protects the file against someone who can write to it.
#![warn(missing_docs)]

use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use silent_sync::{
    bitcoin::{BlockHash, OutPoint, Txid},
    models::{BlockRef, DetectedOutput, EvidenceKind, MatchProvenance},
    store::{check_append, sort_detections, StoreError, StoreMeta, SyncStore},
};
use std::{path::Path, time::Duration};

/// Bumped whenever the table layout changes. Older files are refused.
pub const SCHEMA_VERSION: u32 = 1;

const SCHEMA: &str = "
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) STRICT;
CREATE TABLE blocks (
    height    INTEGER PRIMARY KEY,
    hash      TEXT NOT NULL UNIQUE,
    prev_hash TEXT NOT NULL
) STRICT;
CREATE TABLE detected_outputs (
    txid          TEXT NOT NULL,
    vout          INTEGER NOT NULL,
    height        INTEGER NOT NULL REFERENCES blocks(height),
    value_sats    INTEGER NOT NULL,
    label         INTEGER,
    backend_id    TEXT NOT NULL,
    evidence_kind TEXT NOT NULL,
    PRIMARY KEY (txid, vout)
) STRICT;
CREATE INDEX detected_outputs_by_height ON detected_outputs(height);
";

/// A [`SyncStore`] kept in one SQLite file. See the crate documentation for
/// what the file holds and how it is protected against damage.
pub struct SqliteStore {
    conn: Connection,
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn corrupt(what: &str) -> StoreError {
    StoreError::Invariant(format!("stored {what} is not valid"))
}

impl SqliteStore {
    /// Creates a new database file. Fails if `path` already exists, so an
    /// existing store is never silently replaced.
    pub fn create(path: &Path) -> Result<Self, StoreError> {
        if path.exists() {
            return Err(StoreError::Backend(format!(
                "{} already exists; refusing to overwrite it",
                path.display()
            )));
        }
        let conn = Connection::open(path).map_err(backend)?;
        restrict_permissions(path)?;
        Self::configure(&conn)?;
        conn.execute_batch(SCHEMA).map_err(backend)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(backend)?;
        Ok(Self { conn })
    }

    /// Opens an existing database file. Fails if it does not exist or was
    /// written with another schema version.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if !path.is_file() {
            return Err(StoreError::Backend(format!(
                "{} does not exist; create it with `lab init`",
                path.display()
            )));
        }
        let conn = Connection::open(path).map_err(backend)?;
        Self::configure(&conn)?;
        let store = Self { conn };
        store.check_schema_version()?;
        store.check_integrity()?;
        Ok(store)
    }

    /// A private in-memory database, for tests.
    pub fn in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(backend)?;
        Self::configure(&conn)?;
        conn.execute_batch(SCHEMA).map_err(backend)?;
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )
        .map_err(backend)?;
        Ok(Self { conn })
    }

    fn configure(conn: &Connection) -> Result<(), StoreError> {
        // A second process on the same file waits instead of failing at once.
        conn.busy_timeout(Duration::from_secs(10))
            .map_err(backend)?;
        // Durability over speed: a committed chunk must survive a crash.
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
            .map_err(backend)
    }

    /// Checks, when a file is opened, that what it holds can be a store this
    /// code wrote: a readable binding, and a block chain that starts at the
    /// birthday and is contiguous and hash-linked up to the checkpoint.
    fn check_integrity(&self) -> Result<(), StoreError> {
        let meta = read_meta(&self.conn)?;
        let (count, min, max): (i64, Option<i64>, Option<i64>) = self
            .conn
            .query_row(
                "SELECT COUNT(*), MIN(height), MAX(height) FROM blocks",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(backend)?;
        let Some(meta) = meta else {
            // Not initialized yet: there must be nothing else either.
            return if count == 0 {
                Ok(())
            } else {
                Err(StoreError::Invariant(
                    "stored blocks without a store binding".to_string(),
                ))
            };
        };
        if let (Some(min), Some(max)) = (min, max) {
            if min != i64::from(meta.birthday) || max - min + 1 != count {
                return Err(StoreError::Invariant(format!(
                    "stored chain is not contiguous from the birthday {} (heights {min}..={max}, \
                     {count} blocks)",
                    meta.birthday
                )));
            }
        }
        let broken: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM blocks b JOIN blocks p ON p.height = b.height - 1 \
                 WHERE p.hash != b.prev_hash",
                [],
                |r| r.get(0),
            )
            .map_err(backend)?;
        let self_parent: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM blocks WHERE hash = prev_hash",
                [],
                |r| r.get(0),
            )
            .map_err(backend)?;
        if broken != 0 || self_parent != 0 {
            return Err(StoreError::Invariant(
                "stored chain is not hash-linked".to_string(),
            ));
        }
        // Every stored detection belongs to a stored block and came from the
        // backend the store is bound to (what `check_append` enforced when it
        // was written). A detection without its block would otherwise simply
        // not be listed.
        let orphans: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM detected_outputs d \
                 WHERE NOT EXISTS (SELECT 1 FROM blocks b WHERE b.height = d.height)",
                [],
                |r| r.get(0),
            )
            .map_err(backend)?;
        if orphans != 0 {
            return Err(StoreError::Invariant(format!(
                "{orphans} stored detection(s) refer to blocks that are not stored"
            )));
        }
        let foreign: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM detected_outputs WHERE backend_id != ?1",
                params![meta.backend_id],
                |r| r.get(0),
            )
            .map_err(backend)?;
        if foreign != 0 {
            return Err(StoreError::Invariant(format!(
                "{foreign} stored detection(s) are from another backend than `{}`",
                meta.backend_id
            )));
        }
        Ok(())
    }

    fn check_schema_version(&self) -> Result<(), StoreError> {
        let version: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| StoreError::Backend(format!("not a Silent Sync Lab store: {e}")))?;
        match version.as_deref() {
            Some(v) if v == SCHEMA_VERSION.to_string() => Ok(()),
            other => Err(StoreError::Backend(format!(
                "unsupported store schema version {other:?} (this build reads version {SCHEMA_VERSION})"
            ))),
        }
    }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(backend)
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), StoreError> {
    Ok(())
}

fn read_meta(conn: &Connection) -> Result<Option<StoreMeta>, StoreError> {
    let json: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'store_meta'", [], |r| {
            r.get(0)
        })
        .optional()
        .map_err(backend)?;
    let meta: Option<StoreMeta> = json
        .map(|j| serde_json::from_str(&j).map_err(|_| corrupt("binding")))
        .transpose()?;
    if meta.as_ref().is_some_and(|m| m.birthday == 0) {
        return Err(corrupt("binding (birthday 0)"));
    }
    Ok(meta)
}

fn parse_hash(text: String) -> Result<BlockHash, StoreError> {
    text.parse().map_err(|_| corrupt("block hash"))
}

fn block_from_row(height: i64, hash: String, prev_hash: String) -> Result<BlockRef, StoreError> {
    Ok(BlockRef {
        height: u32::try_from(height).map_err(|_| corrupt("height"))?,
        hash: parse_hash(hash)?,
        prev_hash: parse_hash(prev_hash)?,
    })
}

fn read_checkpoint(conn: &Connection) -> Result<Option<BlockRef>, StoreError> {
    conn.query_row(
        "SELECT height, hash, prev_hash FROM blocks ORDER BY height DESC LIMIT 1",
        [],
        |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        },
    )
    .optional()
    .map_err(backend)?
    .map(|(h, hash, prev)| block_from_row(h, hash, prev))
    .transpose()
}

fn evidence_kind_to_text(kind: EvidenceKind) -> Result<String, StoreError> {
    match serde_json::to_value(kind) {
        Ok(serde_json::Value::String(s)) => Ok(s),
        _ => Err(corrupt("evidence kind")),
    }
}

fn evidence_kind_from_text(text: String) -> Result<EvidenceKind, StoreError> {
    serde_json::from_value(serde_json::Value::String(text)).map_err(|_| corrupt("evidence kind"))
}

/// Reads detections matching `filter` (a trusted SQL fragment, never user input).
fn read_detections(
    conn: &Connection,
    filter: &str,
    bound: i64,
    meta: &StoreMeta,
) -> Result<Vec<DetectedOutput>, StoreError> {
    let sql = format!(
        "SELECT d.txid, d.vout, d.height, b.hash, b.prev_hash, d.value_sats, d.label, \
                d.backend_id, d.evidence_kind \
         FROM detected_outputs d JOIN blocks b ON b.height = d.height \
         WHERE {filter}"
    );
    let mut statement = conn.prepare(&sql).map_err(backend)?;
    type Row = (
        String,
        i64,
        i64,
        String,
        String,
        i64,
        Option<i64>,
        String,
        String,
    );
    let rows = statement
        .query_map(params![bound], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
                r.get(8)?,
            ))
        })
        .map_err(backend)?
        .collect::<Result<Vec<Row>, _>>()
        .map_err(backend)?;

    let mut detections = rows
        .into_iter()
        .map(
            |(txid, vout, height, hash, prev_hash, value, label, backend_id, kind)| {
                Ok(DetectedOutput {
                    network: meta.chain.network(),
                    block: block_from_row(height, hash, prev_hash)?,
                    txid: txid.parse::<Txid>().map_err(|_| corrupt("txid"))?,
                    vout: u32::try_from(vout).map_err(|_| corrupt("vout"))?,
                    value_sats: u64::try_from(value).map_err(|_| corrupt("value"))?,
                    label: label
                        .map(|m| u32::try_from(m).map_err(|_| corrupt("label")))
                        .transpose()?,
                    provenance: MatchProvenance {
                        backend_id,
                        evidence_kind: evidence_kind_from_text(kind)?,
                    },
                })
            },
        )
        .collect::<Result<Vec<DetectedOutput>, StoreError>>()?;
    // The canonical order is defined on `Txid`, not on its hex text, so it is
    // applied here rather than in SQL.
    sort_detections(&mut detections);
    Ok(detections)
}

fn insert_all(
    tx: &Transaction<'_>,
    blocks: &[BlockRef],
    detections: &[DetectedOutput],
) -> Result<(), StoreError> {
    let mut insert_block = tx
        .prepare("INSERT INTO blocks (height, hash, prev_hash) VALUES (?1, ?2, ?3)")
        .map_err(backend)?;
    for b in blocks {
        insert_block
            .execute(params![
                i64::from(b.height),
                b.hash.to_string(),
                b.prev_hash.to_string()
            ])
            .map_err(backend)?;
    }
    let mut insert_detection = tx
        .prepare(
            "INSERT INTO detected_outputs \
             (txid, vout, height, value_sats, label, backend_id, evidence_kind) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(backend)?;
    for d in detections {
        let value = i64::try_from(d.value_sats)
            .map_err(|_| StoreError::Invariant("value does not fit the store".to_string()))?;
        insert_detection
            .execute(params![
                d.txid.to_string(),
                i64::from(d.vout),
                i64::from(d.block.height),
                value,
                d.label.map(i64::from),
                d.provenance.backend_id,
                evidence_kind_to_text(d.provenance.evidence_kind)?,
            ])
            .map_err(backend)?;
    }
    Ok(())
}

impl SyncStore for SqliteStore {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        read_meta(&self.conn)
    }

    fn initialize(&mut self, meta: &StoreMeta) -> Result<(), StoreError> {
        if meta.birthday == 0 {
            return Err(StoreError::Invariant(
                "birthday must be at least 1".to_string(),
            ));
        }
        let json = serde_json::to_string(meta).map_err(backend)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        if read_meta(&tx)?.is_some() {
            return Err(StoreError::AlreadyInitialized);
        }
        tx.execute(
            "INSERT INTO meta (key, value) VALUES ('store_meta', ?1)",
            params![json],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)
    }

    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError> {
        read_checkpoint(&self.conn)
    }

    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError> {
        self.conn
            .query_row(
                "SELECT height, hash, prev_hash FROM blocks WHERE height = ?1",
                params![i64::from(height)],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(backend)?
            .map(|(h, hash, prev)| block_from_row(h, hash, prev))
            .transpose()
    }

    fn append(
        &mut self,
        blocks: &[BlockRef],
        detections: &[DetectedOutput],
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let meta = read_meta(&tx)?;
        let checkpoint = read_checkpoint(&tx)?;
        // Collected up front so the closure below cannot fail.
        let mut stored = std::collections::BTreeSet::new();
        for d in detections {
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM detected_outputs WHERE txid = ?1 AND vout = ?2",
                    params![d.txid.to_string(), i64::from(d.vout)],
                    |r| r.get(0),
                )
                .optional()
                .map_err(backend)?;
            if exists.is_some() {
                stored.insert(OutPoint::new(d.txid, d.vout));
            }
        }
        let mut stored_hashes = std::collections::BTreeSet::new();
        for b in blocks {
            let exists: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM blocks WHERE hash = ?1",
                    params![b.hash.to_string()],
                    |r| r.get(0),
                )
                .optional()
                .map_err(backend)?;
            if exists.is_some() {
                stored_hashes.insert(b.hash);
            }
        }
        check_append(
            meta.as_ref(),
            checkpoint,
            blocks,
            detections,
            &|outpoint| stored.contains(&outpoint),
            &|hash| stored_hashes.contains(&hash),
        )?;
        insert_all(&tx, blocks, detections)?;
        tx.commit().map_err(backend)
    }

    fn rollback_to(&mut self, height: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let meta = read_meta(&tx)?.ok_or(StoreError::NotInitialized)?;
        let removed = read_detections(&tx, "d.height > ?1", i64::from(height), &meta)?;
        tx.execute(
            "DELETE FROM detected_outputs WHERE height > ?1",
            params![i64::from(height)],
        )
        .map_err(backend)?;
        tx.execute(
            "DELETE FROM blocks WHERE height > ?1",
            params![i64::from(height)],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(removed)
    }

    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError> {
        let meta = read_meta(&self.conn)?.ok_or(StoreError::NotInitialized)?;
        read_detections(&self.conn, "d.height >= ?1", 0, &meta)
    }

    fn block_count(&self) -> Result<u64, StoreError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM blocks", [], |r| r.get(0))
            .map_err(backend)?;
        u64::try_from(count).map_err(|_| corrupt("block count"))
    }
}
