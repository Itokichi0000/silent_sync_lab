//! End-to-end tests of the persistent commands (`lab init`, `sync`, `status`,
//! `received list`) on the offline fixture: resume, forced kill, reorg and
//! the refusals that protect the store.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_http::{fixture_blocks, FakeCoreRest},
    fixture_gen,
};
use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_silent-sync");
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/local/regtest_basic_v1.json"
);
const SCAN_PRIVATE_KEY: &str = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";
const SPEND_PRIVATE_KEY: &str = "9d6ad855ce3417ef84e836892e5a56392bfba05fa5d97ccea30e266f540e08b3";

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ssl-cli-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn db(&self) -> String {
        self.0.join("lab.sqlite3").display().to_string()
    }
    fn fork_fixture(&self) -> String {
        let path = self.0.join("fork.json");
        std::fs::write(
            &path,
            serde_json::to_string(&fixture_gen::generate_fork()).unwrap(),
        )
        .unwrap();
        path.display().to_string()
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(BIN).args(args).output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    for text in [&stdout, &stderr] {
        let lower = text.to_lowercase();
        assert!(!lower.contains(SCAN_PRIVATE_KEY) && !lower.contains(SPEND_PRIVATE_KEY));
    }
    assert!(stderr.starts_with("EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS"));
    (output.status.code(), stdout, stderr)
}

fn ok(args: &[&str]) -> String {
    let (code, stdout, stderr) = run(args);
    assert_eq!(code, Some(0), "{args:?}: {stderr}");
    stdout
}

fn json(args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&ok(args)).unwrap()
}

fn init(db: &str, birthday: &str) {
    ok(&[
        "lab",
        "init",
        "--db",
        db,
        "--backend",
        "fixture",
        "--fixture",
        FIXTURE,
        "--birthday",
        birthday,
        "--birthday-basis",
        "fixture chain starts at height 1",
        "--label",
        "1",
    ]);
}

fn sync_args<'a>(db: &'a str, fixture: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "sync",
        "--db",
        db,
        "--backend",
        "fixture",
        "--fixture",
        fixture,
    ];
    args.extend_from_slice(extra);
    args
}

/// (height, txid, vout, value, label) of every stored detection.
fn received(db: &str) -> Vec<(u64, String, u64, u64, Option<u64>)> {
    json(&["received", "list", "--db", db, "--json"])
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["block"]["height"].as_u64().unwrap(),
                d["txid"].as_str().unwrap().to_string(),
                d["vout"].as_u64().unwrap(),
                d["value_sats"].as_u64().unwrap(),
                d["label"].as_u64(),
            )
        })
        .collect()
}

fn checkpoint_height(db: &str) -> Option<u64> {
    json(&["status", "--db", db, "--json"])["checkpoint"]["height"].as_u64()
}

/// What the fixture's sender created for a wallet that knows label 1.
fn expected() -> Vec<(u64, String, u64, u64, Option<u64>)> {
    let tx = |s: &str| s.to_string();
    vec![
        (
            2,
            tx("391d63ac72c9c60e7cb066ad829aadd8cf201e4fff3ad07589d7f705cde04b11"),
            0,
            50_000,
            None,
        ),
        (
            3,
            tx("932699c16df7f7d09801d6ba104af2a2a0358eb53b71449ed6810ac449f395a2"),
            1,
            10_000,
            None,
        ),
        (
            3,
            tx("932699c16df7f7d09801d6ba104af2a2a0358eb53b71449ed6810ac449f395a2"),
            2,
            20_000,
            None,
        ),
        (
            5,
            tx("f9accc13beb1f55d035b11b097ce8c6b5701e9061c6720ea046de93af817a72d"),
            0,
            7_000,
            Some(1),
        ),
        (
            5,
            tx("f9accc13beb1f55d035b11b097ce8c6b5701e9061c6720ea046de93af817a72d"),
            1,
            3_000,
            Some(0),
        ),
        (
            7,
            tx("defc7dcf7f5465f1a54710582fea2cfddb782bb72ae5a2cde98c883b24160a5d"),
            1,
            15_000,
            Some(0),
        ),
        (
            8,
            tx("bfede6f09b7d080f72b553ee2b73f3537ba0c76a57929c3a739baf1bbed61bc8"),
            0,
            1,
            None,
        ),
    ]
}

#[test]
fn init_sync_status_and_received_list() {
    let temp = Temp::new("roundtrip");
    let db = temp.db();
    init(&db, "1");
    assert!(Path::new(&db).is_file());

    let status = json(&["status", "--db", &db, "--json"]);
    assert!(status["checkpoint"].is_null());
    assert_eq!(status["binding"]["labels"], serde_json::json!([0, 1]));
    assert_eq!(status["binding"]["birthday"], 1);
    assert_eq!(
        status["binding"]["birthday_basis"],
        "fixture chain starts at height 1"
    );
    assert_eq!(status["binding"]["backend_id"], "fixture");
    assert_eq!(status["binding"]["chain"]["network"], "regtest");
    assert!(received(&db).is_empty());

    let report = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(report["caught_up"], true);
    assert_eq!(report["blocks_scanned"], 8);
    assert!(report["resumed_from"].is_null() && report["reorg"].is_null());
    assert_eq!(report["new_detections"].as_array().unwrap().len(), 7);
    assert_eq!(report["checkpoint"]["height"], 8);

    assert_eq!(received(&db), expected());
    let status = json(&["status", "--db", &db, "--json"]);
    assert_eq!(status["checkpoint"]["height"], 8);
    assert_eq!(status["blocks_stored"], 8);
    assert_eq!(status["detected_received_outputs"], 7);

    // Human-readable forms never talk about a balance.
    for text in [
        ok(&["status", "--db", &db]),
        ok(&["received", "list", "--db", &db]),
        ok(&sync_args(&db, FIXTURE, &[])),
    ] {
        assert!(text.contains("not a balance"), "{text}");
        assert!(!text.to_lowercase().contains("total"), "{text}");
    }

    // Syncing again scans nothing and changes nothing.
    let again = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(again["blocks_scanned"], 0);
    assert_eq!(again["resumed_from"]["height"], 8);
    assert_eq!(received(&db), expected());
}

#[test]
fn stopped_sync_resumes_from_the_checkpoint() {
    let temp = Temp::new("resume");
    let db = temp.db();
    init(&db, "1");
    let half = json(&sync_args(
        &db,
        FIXTURE,
        &["--chunk-size", "4", "--max-blocks", "4", "--json"],
    ));
    assert_eq!(half["caught_up"], false);
    assert_eq!(half["stopped_because"], "max_blocks_reached");
    assert_eq!(checkpoint_height(&db), Some(4));
    assert_eq!(received(&db), expected()[..3].to_vec());
    let text = ok(&sync_args(&db, FIXTURE, &["--to", "6"]));
    assert!(
        text.contains("resumed from         : 4")
            && text.contains("caught up            : no (stopped at --to, below the source tip"),
        "{text}"
    );
    let rest = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(rest["resumed_from"]["height"], 6);
    assert_eq!(rest["blocks_scanned"], 2);
    assert_eq!(received(&db), expected());
}

/// Gate 3: kill the process (SIGKILL) in the middle of a sync, then resume.
#[test]
fn killed_sync_resumes_without_loss_or_duplicates() {
    let temp = Temp::new("kill");
    let db = temp.db();
    init(&db, "1");

    let mut child = Command::new(BIN)
        .args(sync_args(
            &db,
            FIXTURE,
            &["--chunk-size", "1", "--chunk-delay-ms", "400"],
        ))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = BufReader::new(child.stderr.take().unwrap());
    for line in stderr.lines() {
        if line.unwrap().contains("committed up to height 3") {
            break;
        }
    }
    child.kill().unwrap(); // SIGKILL: no cleanup code runs
    let status = child.wait().unwrap();
    assert!(!status.success());

    let at_kill = checkpoint_height(&db).unwrap();
    assert!((3..8).contains(&at_kill), "killed at checkpoint {at_kill}");
    let partial = received(&db);
    let want: Vec<_> = expected().into_iter().filter(|d| d.0 <= at_kill).collect();
    assert_eq!(
        partial, want,
        "store after the kill must be exactly the scanned prefix"
    );

    let report = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(report["caught_up"], true);
    assert_eq!(report["resumed_from"]["height"], at_kill);
    assert_eq!(report["blocks_scanned"], 8 - at_kill);
    assert_eq!(
        received(&db),
        expected(),
        "no loss and no duplicates after resuming"
    );
}

#[test]
fn reorg_is_reported_and_resolved() {
    let temp = Temp::new("reorg");
    let db = temp.db();
    let fork = temp.fork_fixture();
    init(&db, "1");
    ok(&sync_args(&db, FIXTURE, &[]));
    assert_eq!(received(&db), expected());

    // The source now serves a competing chain that diverges after height 5.
    let report = json(&sync_args(&db, &fork, &["--json"]));
    assert_eq!(report["reorg"]["fork_height"], 5);
    assert_eq!(report["reorg"]["blocks_rolled_back"], 3);
    assert_eq!(report["reorg"]["abandoned_checkpoint"]["height"], 8);
    assert_eq!(
        report["reorg"]["detections_removed"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(report["blocks_scanned"], 4);
    assert_eq!(report["checkpoint"]["height"], 9);

    let after = received(&db);
    assert_eq!(after.len(), 7);
    assert_eq!(
        after[..5],
        expected()[..5],
        "blocks up to the fork point are untouched"
    );
    assert_eq!(
        (after[5].0, after[5].3),
        (6, 33_000),
        "payment that exists only on the fork"
    );
    assert_eq!(
        (after[6].0, &after[6].1, after[6].3),
        (8, &expected()[6].1, 1),
        "re-detected"
    );
    assert!(
        !after.iter().any(|d| d.3 == 15_000),
        "the orphaned payment is gone"
    );

    // Text form says so explicitly.
    let back = ok(&sync_args(&db, FIXTURE, &[]));
    assert!(
        back.contains("REORG")
            && back.contains("rolled back to height 5 (4 blocks, 2 detections removed)"),
        "{back}"
    );
    assert_eq!(received(&db), expected());
}

#[test]
fn birthday_limits_what_is_found() {
    let temp = Temp::new("birthday");
    let db = temp.db();
    init(&db, "4");
    let report = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(report["blocks_scanned"], 5);
    assert_eq!(received(&db), expected()[3..].to_vec());
    let status = ok(&["status", "--db", &db]);
    assert!(status.contains("birthday             : 4"), "{status}");
}

#[test]
fn the_store_is_protected_from_misuse() {
    let temp = Temp::new("refusals");
    let db = temp.db();
    let refused = |args: &[&str], needle: &str| {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, Some(1), "{args:?}: {stderr}");
        assert!(stdout.is_empty(), "{args:?} printed: {stdout}");
        assert!(
            stderr.contains(needle),
            "{args:?}: `{stderr}` should mention `{needle}`"
        );
    };

    // Nothing exists yet.
    refused(&sync_args(&db, FIXTURE, &[]), "does not exist");
    refused(&["status", "--db", &db], "does not exist");
    refused(&["received", "list", "--db", &db], "does not exist");

    // Bad init arguments leave no file behind.
    let init_with = |birthday: &'static str, basis: &'static str| {
        vec![
            "lab",
            "init",
            "--db",
            db.as_str(),
            "--backend",
            "fixture",
            "--fixture",
            FIXTURE,
            "--birthday",
            birthday,
            "--birthday-basis",
            basis,
        ]
    };
    refused(&init_with("0", "x"), "birthday must be at least 1");
    refused(&init_with("1", "  "), "must say why");
    refused(
        &init_with("1", "line one\nline two\u{1b}[31m"),
        "without control characters",
    );
    let long: &'static str = Box::leak("x".repeat(501).into_boxed_str());
    refused(&init_with("1", long), "at most 500 characters");
    assert!(!Path::new(&db).exists());

    init(&db, "1");
    ok(&sync_args(&db, FIXTURE, &[]));
    let before = std::fs::read(&db).unwrap();

    // An existing store is never overwritten.
    refused(&init_with("1", "again"), "refusing to overwrite");
    // Wrong network for this store.
    refused(
        &["--network", "signet", "status", "--db", &db],
        "is for regtest",
    );
    refused(
        &["--network", "signet", "received", "list", "--db", &db],
        "is for regtest",
    );
    // Limits that make no sense.
    refused(
        &sync_args(&db, FIXTURE, &["--to", "99"]),
        "above the source's tip",
    );
    refused(
        &sync_args(&db, FIXTURE, &["--chunk-size", "0"]),
        "chunk size",
    );
    // A limit that is refused is refused before a reorg is acted on: the
    // other chain is offered together with an impossible target.
    let fork = temp.fork_fixture();
    refused(
        &sync_args(&db, &fork, &["--to", "99"]),
        "above the source's tip",
    );
    refused(
        &sync_args(&db, &fork, &["--to", "99"]),
        "the store was not changed",
    );
    // A different backend than the store was created with: a reachable one
    // is refused by the store's binding ...
    let node = FakeCoreRest::serve(&fixture_gen::generate());
    refused(
        &[
            "sync",
            "--db",
            &db,
            "--backend",
            "core",
            "--core-rest-url",
            &node.url(),
        ],
        "the store is bound to a different backend: stored `fixture`, given `bitcoin_core_rest`",
    );
    // ... and an unreachable one before that.
    refused(
        &[
            "sync",
            "--db",
            &db,
            "--backend",
            "core",
            "--core-rest-url",
            "http://127.0.0.1:9",
        ],
        "source is unavailable",
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        before,
        "refused commands must not modify the store"
    );

    // Mainnet is refused for the persistent commands too.
    for args in [
        vec!["--network", "mainnet", "status", "--db", db.as_str()],
        vec![
            "--network",
            "mainnet",
            "received",
            "list",
            "--db",
            db.as_str(),
        ],
        vec![
            "--network",
            "bitcoin",
            "sync",
            "--db",
            db.as_str(),
            "--backend",
            "fixture",
            "--fixture",
            FIXTURE,
        ],
        vec![
            "--network",
            "main",
            "lab",
            "init",
            "--db",
            "/nonexistent/x",
            "--backend",
            "fixture",
            "--fixture",
            FIXTURE,
            "--birthday",
            "1",
            "--birthday-basis",
            "x",
        ],
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(code, Some(2), "{args:?}");
        assert!(
            stdout.is_empty() && stderr.contains("mainnet is refused"),
            "{args:?}: {stderr}"
        );
    }
}

#[test]
fn the_store_file_contains_no_key_material() {
    let temp = Temp::new("secrets");
    let db = temp.db();
    init(&db, "1");
    ok(&sync_args(&db, FIXTURE, &[]));
    let bytes = std::fs::read(&db).unwrap();
    let text = String::from_utf8_lossy(&bytes).to_lowercase();
    for secret in [SCAN_PRIVATE_KEY, SPEND_PRIVATE_KEY] {
        assert!(!text.contains(secret));
        let raw: Vec<u8> = (0..32)
            .map(|i| u8::from_str_radix(&secret[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        assert!(!bytes.windows(32).any(|w| w == raw.as_slice()));
    }
    for forbidden in ["tweak", "scan_sk", "priv", "seed"] {
        assert!(!text.contains(forbidden), "{forbidden}");
    }
}

/// The reasons a sync is not caught up are spelled out, not guessed.
#[test]
fn the_reason_for_stopping_is_shown() {
    let temp = Temp::new("stop-reason");
    let db = temp.db();
    init(&db, "20");
    let report = json(&sync_args(&db, FIXTURE, &["--json"]));
    assert_eq!(report["caught_up"], false);
    assert_eq!(report["stopped_because"], "birthday_above_tip");
    assert_eq!(report["blocks_scanned"], 0);
    let text = ok(&sync_args(&db, FIXTURE, &[]));
    assert!(
        text.contains("caught up            : no (the birthday is above the source tip"),
        "{text}"
    );

    let other = Temp::new("stop-reason-2");
    let db = other.db();
    init(&db, "1");
    let text = ok(&sync_args(
        &db,
        FIXTURE,
        &["--chunk-size", "2", "--max-blocks", "2"],
    ));
    assert!(
        text.contains("caught up            : no (stopped by --max-blocks"),
        "{text}"
    );
    let done = json(&sync_args(&db, FIXTURE, &["--to", "8", "--json"]));
    assert_eq!(done["stopped_because"], "caught_up");
    assert_eq!(done["caught_up"], true);
}

/// A store file that `lab init` did not finish is named as such.
#[test]
fn a_store_file_without_a_binding_is_explained() {
    let temp = Temp::new("unbound");
    let db = temp.db();
    drop(silent_sync_sqlite::SqliteStore::create(Path::new(&db)).unwrap());
    for args in [
        vec!["status", "--db", db.as_str()],
        vec!["received", "list", "--db", db.as_str()],
        sync_args(&db, FIXTURE, &[]),
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(code, Some(1));
        assert!(
            stdout.is_empty() && stderr.contains("store file without a binding"),
            "{args:?}: {stderr}"
        );
    }
}

/// A sync that fails after it changed the store — here: a reorg was rolled
/// back, one block of the new chain was committed, then the node stopped
/// serving blocks — prints what it changed before it reports the failure.
#[test]
fn an_interrupted_sync_reports_what_it_changed() {
    let temp = Temp::new("interrupted");
    let db = temp.db();
    let main = fixture_gen::generate();
    let fork = fixture_gen::generate_fork();
    let mut node = FakeCoreRest::serve(&main);
    let url = node.url();
    let core = |extra: &[&'static str]| -> Vec<String> {
        let mut args: Vec<String> = [
            "sync",
            "--db",
            &db,
            "--backend",
            "core",
            "--core-rest-url",
            &url,
        ]
        .iter()
        .map(|a| a.to_string())
        .collect();
        args.extend(extra.iter().map(|a| a.to_string()));
        args
    };
    let call = |args: Vec<String>| {
        let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
        run(&borrowed)
    };

    ok(&[
        "lab",
        "init",
        "--db",
        &db,
        "--backend",
        "core",
        "--core-rest-url",
        &url,
        "--birthday",
        "1",
        "--birthday-basis",
        "fixture chain starts at height 1",
        "--label",
        "1",
    ]);
    let (code, _, stderr) = call(core(&[]));
    assert_eq!(code, Some(0), "{stderr}");
    assert_eq!(received(&db), expected());

    // Reorg to the fork; the node then fails to serve block 7 of it.
    node.replace_chain(fork.network, &fixture_blocks(&fork));
    let block_7 = node.block_path(7);
    let saved = node.get(&block_7);
    node.remove(&block_7);

    let (code, stdout, stderr) = call(core(&["--chunk-size", "1", "--json"]));
    assert_eq!(code, Some(1), "{stderr}");
    let partial: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(partial["stopped_because"], "error");
    assert_eq!(partial["caught_up"], false);
    assert_eq!(partial["reorg"]["fork_height"], 5);
    assert_eq!(partial["reorg"]["blocks_rolled_back"], 3);
    assert_eq!(
        partial["reorg"]["detections_removed"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(partial["blocks_scanned"], 1);
    assert_eq!(partial["checkpoint"]["height"], 6);
    assert_eq!(partial["new_detections"].as_array().unwrap().len(), 1);
    assert!(
        stderr.contains("sync aborted after changing the store")
            && stderr.contains("source is unavailable"),
        "{stderr}"
    );
    // The store is exactly what was reported.
    assert_eq!(checkpoint_height(&db), Some(6));
    let stored = received(&db);
    assert_eq!(stored.len(), 6);
    assert_eq!(stored[..5], expected()[..5]);
    assert_eq!((stored[5].0, stored[5].3), (6, 33_000));

    // Failing again without changing anything more is a plain refusal.
    let (code, stdout, stderr) = call(core(&["--chunk-size", "1"]));
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("sync aborted: the store was not changed"),
        "{stderr}"
    );

    // Text form of an interruption: roll the store onto the main chain again,
    // failing at its block 8.
    node.replace_chain(main.network, &fixture_blocks(&main));
    let block_8 = node.block_path(8);
    node.remove(&block_8);
    let (code, stdout, stderr) = call(core(&["--chunk-size", "2"]));
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stdout.contains("REORG")
            && stdout.contains("rolled back to height 5 (1 blocks, 1 detections removed)")
            && stdout.contains("caught up            : no (INTERRUPTED by an error"),
        "{stdout}"
    );
    assert!(
        stderr.contains("sync aborted after changing the store"),
        "{stderr}"
    );
    // Blocks 6 and 7 of the main chain were committed before block 8 failed.
    assert_eq!(checkpoint_height(&db), Some(7));
    assert_eq!(received(&db), expected()[..6].to_vec());

    // Once the node serves everything, the sync completes.
    node.replace_chain(fork.network, &fixture_blocks(&fork));
    node.set(&block_7, saved);
    let (code, stdout, stderr) = call(core(&["--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    let done: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(done["reorg"]["blocks_rolled_back"], 2);
    assert_eq!(done["stopped_because"], "caught_up");
    assert_eq!(done["caught_up"], true);
    assert_eq!(done["checkpoint"]["height"], 9);
    assert_eq!(received(&db).len(), 7); // Nothing is left to report about the reorg afterwards.
    let (code, stdout, stderr) = call(core(&["--json"]));
    assert_eq!(code, Some(0), "{stderr}");
    let again: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert!(again["reorg"].is_null());
    assert_eq!(again["blocks_scanned"], 0);
}
