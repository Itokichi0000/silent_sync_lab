//! `silent-sync bench` and `export` on the offline fixture: every run is
//! recorded, results are compared across backends and modes, failures are not
//! hidden, and the results hold no personal data.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::fixture_gen;
use std::{path::PathBuf, process::Command};

const BIN: &str = env!("CARGO_BIN_EXE_silent-sync");
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/local/regtest_basic_v1.json"
);
const SCAN_PRIVATE_KEY: &str = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";

struct Temp(PathBuf);

impl Temp {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ssl-bench-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }
    /// Files left in the directory (temporary stores must be cleaned up).
    fn entries(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
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
    assert!(stderr.starts_with("EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS"));
    (output.status.code(), stdout, stderr)
}

fn bench<'a>(temp: &'a Temp, output: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "bench",
        "--fixture",
        FIXTURE,
        "--label",
        "1",
        "--output",
        output,
        "--work-dir",
        temp.0.to_str().unwrap(),
    ];
    args.extend_from_slice(extra);
    args
}

fn read(path: &str) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn every_run_is_recorded_and_compared() {
    let temp = Temp::new("all");
    let out = temp.path("results.json");
    let (code, stdout, stderr) = run(&bench(
        &temp,
        &out,
        &[
            "--backends",
            "fixture,local-index",
            "--repeat",
            "2",
            "--note",
            "unit test\nsecond line",
        ],
    ));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("client-side measurements only") && stdout.contains("sync_resume"),
        "{stdout}"
    );
    // Only the results file is left: temporary stores are removed.
    assert_eq!(temp.entries(), vec!["results.json"]);

    let file = read(&out);
    assert_eq!(file["format"], "silent-sync-lab/benchmark-v1");
    assert_eq!(file["environment"]["notes"][0], "unit test second line");
    assert!(file["environment"]["rustc"]
        .as_str()
        .unwrap()
        .starts_with("rustc "));
    assert!(file["environment"]["scanner_version"]
        .as_str()
        .unwrap()
        .contains("bdk_sp@"));

    let runs = file["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 2 * 5 * 2, "backends x modes x repeats");
    for (index, r) in runs.iter().enumerate() {
        assert_eq!(r["order"], index as u64 + 1);
        assert!(r["error"].is_null(), "{r}");
        assert_eq!((&r["from_height"], &r["to_height"]), (&1.into(), &8.into()));
        assert_eq!(r["network"], "regtest");
        assert_eq!(r["detected_count"], 7);
        assert_eq!(r["labels_scanned"], serde_json::json!([0, 1]));
        assert!(r["wall_ms"].as_f64().unwrap() > 0.0);
        assert!(r["wall_ms"].as_f64().unwrap() >= r["connect_ms"].as_f64().unwrap());
        assert!(r["server_preprocess_ms"].is_null());
        assert_eq!(
            r["verification_status"],
            if index == 0 {
                "reference"
            } else {
                "same_as_reference"
            }
        );
        // What each mode actually measured.
        let (blocks, measured_from) = match r["mode"].as_str().unwrap() {
            "scan" | "sync_cold" | "restore" => (8, 1),
            "sync_resume" => (4, 5),
            "sync_noop" => (0, 9),
            other => panic!("unknown mode {other}"),
        };
        assert_eq!(r["blocks"], blocks, "{r}");
        assert_eq!(r["measured_from_height"], measured_from, "{r}");
    }
    // Round-robin: all modes of all backends once, then again.
    let order: Vec<(String, String, u64)> = runs
        .iter()
        .map(|r| {
            (
                r["backend"].as_str().unwrap().to_string(),
                r["mode"].as_str().unwrap().to_string(),
                r["repeat"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(order[0], ("fixture".into(), "scan".into(), 1));
    assert_eq!(order[5], ("local-index".into(), "scan".into(), 1));
    assert_eq!(order[10], ("fixture".into(), "scan".into(), 2));
    assert_eq!(runs[0]["evidence_kind"], "full_block_with_prevouts");
    assert_eq!(runs[5]["evidence_kind"], "tweak_index");
    assert_eq!(runs[5]["backend_id"], "local_tweak_index");
    if cfg!(target_os = "linux") {
        assert!(runs[0]["cpu_ms"].is_number());
    }

    // An existing results file is never overwritten.
    let before = std::fs::read(&out).unwrap();
    let (code, stdout, stderr) = run(&bench(&temp, &out, &["--backends", "fixture"]));
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("never overwritten"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&out).unwrap(), before);
}

/// Results are meant to be shared: they must not identify the wallet or the
/// machine, and of course hold no key.
#[test]
fn results_hold_no_personal_data() {
    let temp = Temp::new("privacy");
    let out = temp.path("results.json");
    let (code, _, stderr) = run(&bench(
        &temp,
        &out,
        &["--backends", "fixture", "--repeat", "1"],
    ));
    assert_eq!(code, Some(0), "{stderr}");
    let csv = temp.path("results.csv");
    let md = temp.path("results.md");
    for (format, path) in [("csv", &csv), ("markdown", &md)] {
        let (code, stdout, stderr) = run(&[
            "export", "--input", &out, "--format", format, "--output", path,
        ]);
        assert_eq!(code, Some(0), "{stderr}");
        assert!(stdout.is_empty());
    }

    let fixture = fixture_gen::generate();
    let wallet_id = fixture_gen::lab_receiver_keys().wallet_id();
    let home = temp.0.display().to_string();
    for path in [&out, &csv, &md] {
        let text = std::fs::read_to_string(path).unwrap().to_lowercase();
        for detection in &fixture.expected_detections {
            assert!(
                !text.contains(&detection.txid.to_string()),
                "{path} names a transaction of the wallet"
            );
        }
        assert!(!text.contains(&wallet_id), "{path} holds the wallet id");
        assert!(!text.contains(SCAN_PRIVATE_KEY), "{path} holds a key");
        assert!(!text.contains(&home.to_lowercase()), "{path} holds a path");
        assert!(
            !text.contains("regtest_basic_v1"),
            "{path} holds a file name"
        );
        assert!(
            !text.contains("127.0.0.1") && !text.contains("http://"),
            "{path} holds a URL"
        );
    }
}

/// A backend that cannot be used is recorded as failed runs — it neither
/// stops the other backends nor lets the command look successful.
#[test]
fn failed_and_differing_runs_are_recorded_and_fail_the_command() {
    let temp = Temp::new("failures");
    let out = temp.path("failed.json");
    let (code, stdout, stderr) = run(&bench(
        &temp,
        &out,
        &[
            "--backends",
            "fixture,core",
            "--core-rest-url",
            "http://127.0.0.1:9",
            "--repeat",
            "1",
            "--modes",
            "scan,sync_cold",
        ],
    ));
    assert_eq!(code, Some(1), "{stdout}");
    assert!(stderr.contains("2 run(s) failed"), "{stderr}");
    // The reason itself is shown on the terminal.
    assert!(
        stderr.contains("FAILED (source could not be opened): source is unavailable"),
        "{stderr}"
    );
    let runs = read(&out)["runs"].as_array().unwrap().clone();
    assert_eq!(runs.len(), 4);
    for r in &runs[..2] {
        assert!(r["error"].is_null());
        assert_eq!(r["detected_count"], 7);
    }
    for r in &runs[2..] {
        assert_eq!(r["backend"], "core");
        // The class of the failure, not its text.
        assert_eq!(r["error"], "source could not be opened", "{r}");
        assert_eq!(r["verification_status"], "not_run");
        assert_eq!(r["detected_count"], 0);
        assert!(!r["error"].as_str().unwrap().contains('\n'));
    }
    assert_eq!(temp.entries(), vec!["failed.json"]);

    // An index that lists a transaction BIP352 excludes finds one output
    // more: the run is marked as differing and the command fails.
    let out = temp.path("differs.json");
    let (code, _, stderr) = run(&bench(
        &temp,
        &out,
        &[
            "--backends",
            "fixture,local-index",
            "--index-policy",
            "ignore-segwit-version-rule",
            "--repeat",
            "1",
            "--modes",
            "scan",
        ],
    ));
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("1 run(s) produced a different result"),
        "{stderr}"
    );
    let runs = read(&out)["runs"].as_array().unwrap().clone();
    assert_eq!(runs[0]["verification_status"], "reference");
    assert_eq!(runs[1]["verification_status"], "differs_from_reference");
    assert_eq!(
        (&runs[0]["detected_count"], &runs[1]["detected_count"]),
        (&7.into(), &8.into())
    );

    // Arguments that make no sense are refused before anything runs.
    for (extra, needle) in [
        (vec!["--backends", "fixture", "--repeat", "0"], "at least 1"),
        (
            vec!["--backends", "fixture", "--modes", "scan,warp"],
            "unknown mode `warp`",
        ),
        (
            vec!["--backends", "fixture", "--from", "5", "--to", "4"],
            "invalid range 5..=4",
        ),
    ] {
        let out = temp.path("never.json");
        let (code, stdout, stderr) = run(&bench(&temp, &out, &extra));
        assert_eq!(code, Some(1), "{extra:?}");
        assert!(stdout.is_empty() && stderr.contains(needle), "{stderr}");
        assert!(!std::path::Path::new(&out).exists());
    }
    // Mainnet is refused here as everywhere.
    let out = temp.path("never.json");
    let mut args = vec!["--network", "mainnet"];
    args.extend(bench(&temp, &out, &["--backends", "fixture"]));
    let (code, _, stderr) = run(&args);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("mainnet is refused"), "{stderr}");
}

#[test]
fn export_converts_and_merges_results() {
    let temp = Temp::new("export");
    let (a, b) = (temp.path("a.json"), temp.path("b.json"));
    for (out, to) in [(&a, "4"), (&b, "8")] {
        let (code, _, stderr) = run(&bench(
            &temp,
            out,
            &[
                "--backends",
                "fixture",
                "--repeat",
                "3",
                "--modes",
                "scan,sync_cold",
                "--to",
                to,
                "--note",
                "same note",
            ],
        ));
        assert_eq!(code, Some(0), "{stderr}");
    }

    // JSON export of one file is the file.
    let (code, stdout, _) = run(&["export", "--input", &a]);
    assert_eq!(code, Some(0));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stdout).unwrap(),
        read(&a)
    );

    // Two files merge into one, in order, without losing a run.
    let (code, stdout, stderr) = run(&["export", "--input", &a, "--input", &b]);
    assert_eq!(code, Some(0), "{stderr}");
    let merged: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let runs = merged["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 12);
    assert_eq!(
        runs.iter()
            .map(|r| r["order"].as_u64().unwrap())
            .collect::<Vec<_>>(),
        (1..=12).collect::<Vec<_>>()
    );
    assert_eq!(runs[5]["to_height"], 4);
    assert_eq!(runs[6]["to_height"], 8);
    assert_eq!(
        merged["environment"]["notes"],
        serde_json::json!(["same note"])
    );

    let (code, csv, _) = run(&["export", "--input", &a, "--input", &b, "--format", "csv"]);
    assert_eq!(code, Some(0));
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 13);
    assert!(lines[0].starts_with("run_id,order,backend,backend_id,mode,"));
    let columns = lines[0].split(',').count();
    assert!(lines[1].starts_with("fixture-scan-1-4-r1,1,fixture,fixture,scan,regtest,"));
    // The cache condition contains commas and is quoted; nothing else does.
    assert_eq!(lines[1].split('"').count(), 3, "{}", lines[1]);
    assert_eq!(
        lines[1].split('"').next().unwrap().split(',').count(),
        columns
    );

    let (code, md, _) = run(&[
        "export", "--input", &a, "--input", &b, "--format", "markdown",
    ]);
    assert_eq!(code, Some(0));
    for needle in [
        "# Silent Sync Lab — benchmark results",
        "EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS",
        "| 1..=4 | 4 | fixture | scan | 3 | 0 |",
        "| 1..=8 | 8 | fixture | sync_cold | 3 | 0 |",
        "**not included**",
        "say nothing about performance over the internet",
        "| note | same note |",
        "## All runs, in execution order",
    ] {
        assert!(md.contains(needle), "missing `{needle}` in:\n{md}");
    }
    assert_eq!(md.matches("| fixture | scan | 1..=").count(), 6);

    // Output files are never overwritten; other files are not results.
    let (code, _, stderr) = run(&["export", "--input", &a, "--output", &b]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("never overwritten"), "{stderr}");
    assert_eq!(read(&b)["runs"].as_array().unwrap().len(), 6);
    let (code, _, stderr) = run(&["export", "--input", FIXTURE]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("is not a benchmark results file"),
        "{stderr}"
    );
    let other = temp.path("other.json");
    std::fs::write(
        &other,
        std::fs::read_to_string(&a)
            .unwrap()
            .replace("benchmark-v1", "benchmark-v9"),
    )
    .unwrap();
    let (code, _, stderr) = run(&["export", "--input", &other]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("unsupported results format"), "{stderr}");
}

/// Failed runs must not smuggle what successful runs leave out: error text
/// can quote a path, a URL or a transaction id, so only its class is kept.
#[test]
fn failed_runs_record_a_class_not_the_error_text() {
    let temp = Temp::new("failure-privacy");
    let out = temp.path("results.json");
    let missing_dir = temp.path("alice-private-dir/does-not-exist");
    let (code, _, stderr) = run(&[
        "bench",
        "--backends",
        "fixture,core",
        "--fixture",
        FIXTURE,
        "--core-rest-url",
        "http://mynode.home.example:8332",
        "--label",
        "1",
        "--repeat",
        "1",
        "--modes",
        "scan,sync_cold",
        "--to",
        "8",
        "--output",
        &out,
        "--work-dir",
        &missing_dir,
    ]);
    assert_eq!(code, Some(1));
    // The terminal shows the details ...
    assert!(
        stderr.contains("alice-private-dir") && stderr.contains("mynode.home.example"),
        "{stderr}"
    );
    // ... the results and every export of them do not.
    let csv = temp.path("results.csv");
    let md = temp.path("results.md");
    for (format, path) in [("csv", &csv), ("markdown", &md)] {
        let (code, _, stderr) = run(&[
            "export", "--input", &out, "--format", format, "--output", path,
        ]);
        assert_eq!(code, Some(0), "{stderr}");
    }
    for path in [&out, &csv, &md] {
        let text = std::fs::read_to_string(path).unwrap();
        for needle in [
            "alice-private-dir",
            "mynode",
            "example",
            "http://",
            "sqlite3",
        ] {
            assert!(!text.contains(needle), "{path} contains `{needle}`");
        }
    }
    let errors: Vec<Option<String>> = read(&out)["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["error"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        errors,
        vec![
            None,                                           // fixture scan needs no store
            Some("store: backend error".to_string()),       // fixture sync_cold: no such directory
            Some("source could not be opened".to_string()), // core: URL refused
            Some("source could not be opened".to_string()),
        ]
    );
}

/// The request and byte counters cover everything the measured part sent and
/// received, connecting included.
#[test]
fn transfer_counters_include_the_connection() {
    use lab_testkit::fake_http::FakeCoreRest;
    let temp = Temp::new("counters");
    let out = temp.path("results.json");
    let node = FakeCoreRest::serve(&fixture_gen::generate());
    let (code, _, stderr) = run(&[
        "bench",
        "--backends",
        "core",
        "--core-rest-url",
        &node.url(),
        "--label",
        "1",
        "--repeat",
        "1",
        "--modes",
        "scan",
        "--from",
        "1",
        "--to",
        "8",
        "--output",
        &out,
        "--work-dir",
        temp.0.to_str().unwrap(),
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    let run = &read(&out)["runs"][0];
    let served = node.http.requests();
    // After the measured part, the range's first and last block hashes are
    // looked up for the record: those two requests are not part of it.
    let (measured, afterwards) = served.split_at(served.len() - 2);
    assert!(
        afterwards
            .iter()
            .all(|r| r.starts_with("/rest/blockhashbyheight/")),
        "{afterwards:?}"
    );
    assert_eq!(
        run["requests"].as_u64().unwrap(),
        measured.len() as u64,
        "every request of the measured part is counted: {measured:?}"
    );
    assert_eq!(
        measured[0], "/rest/chaininfo.json",
        "the connection's own requests are included"
    );
    // 8 blocks and their spent outputs, plus the connection and header checks.
    assert!(served.len() > 16, "{served:?}");
    assert!(run["downloaded_bytes"].as_u64().unwrap() > 1_000);
    assert_eq!(run["detected_count"], 7);
}
