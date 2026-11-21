//! End-to-end tests of the tweak-index backends and `verify` through the CLI,
//! on the fixture and on in-process fakes of the node and the oracle.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_http::{FakeCoreRest, FakeOracle},
    fixture_gen,
};
use std::process::Command;
use sync_core::source::{
    fixture::FixtureSource,
    local_index::{IndexPolicy, LocalTweakIndex},
    ScanSource,
};

const BIN: &str = env!("CARGO_BIN_EXE_silent-sync");
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/local/regtest_basic_v1.json"
);
const SCAN_PRIVATE_KEY: &str = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";
const SPEND_PRIVATE_KEY: &str = "9d6ad855ce3417ef84e836892e5a56392bfba05fa5d97ccea30e266f540e08b3";

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

fn json(args: &[&str], expected_code: i32) -> serde_json::Value {
    let (code, stdout, stderr) = run(args);
    assert_eq!(code, Some(expected_code), "{args:?}: {stderr}");
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{args:?}: {e}: {stdout}"))
}

/// A fake node and a fake oracle (with the observed lenient policy) for the fixture.
fn refs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}

fn services() -> (FakeCoreRest, FakeOracle) {
    let fixture = fixture_gen::generate();
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture.clone()).unwrap(),
        IndexPolicy::IgnoreSegwitVersionRule,
    )
    .fetch_range(1, 8)
    .unwrap();
    (
        FakeCoreRest::serve(&fixture),
        FakeOracle::serve("regtest", &index.blocks),
    )
}

#[test]
fn local_index_scan_and_verify_on_the_fixture() {
    let scan = json(
        &[
            "scan",
            "--backend",
            "local-index",
            "--fixture",
            FIXTURE,
            "--label",
            "1",
            "--json",
        ],
        0,
    );
    assert_eq!(scan["backend_id"], "local_tweak_index");
    assert_eq!(scan["evidence_kind"], "tweak_index");
    let detections = scan["detected_received_outputs"].as_array().unwrap();
    assert_eq!(detections.len(), 7);
    assert!(detections
        .iter()
        .all(|d| d["provenance"]["evidence_kind"] == "tweak_index"));

    // A BIP352-conformant index agrees with scanning full blocks.
    let (code, stdout, stderr) = run(&[
        "verify",
        "--backend",
        "local-index",
        "--fixture",
        FIXTURE,
        "--reference",
        "fixture",
        "--label",
        "1",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("agreed outputs       : 7")
            && stdout.contains("verdict              : identical"),
        "{stdout}"
    );
    assert!(stdout.contains("does not prove either is complete"));

    // One that ignores the SegWit version rule does not, and the report says why.
    let args = [
        "verify",
        "--backend",
        "local-index",
        "--index-policy",
        "ignore-segwit-version-rule",
        "--fixture",
        FIXTURE,
        "--reference",
        "fixture",
        "--label",
        "1",
    ];
    let (code, stdout, stderr) = run(&args);
    assert_eq!(
        code,
        Some(4),
        "differences must not exit successfully: {stderr}"
    );
    assert!(
        stdout.contains("only in candidate    : 1") && stdout.contains("DIFFERENT"),
        "{stdout}"
    );
    assert!(stdout.contains("spends_unknown_segwit_version"), "{stdout}");
    assert!(
        stderr.contains(
            "the backends differ: 1 difference(s) (1 only in the candidate, 0 only in the \
             reference, 0 unexplained), 0 block(s) with different hashes"
        ),
        "{stderr}"
    );
    let mut with_json = args.to_vec();
    with_json.push("--json");
    let report = json(&with_json, 4);
    assert_eq!(report["identical"], false);
    assert_eq!(
        report["differences"][0]["explanation"]["kind"],
        "reference_not_eligible"
    );
    assert_eq!(report["differences"][0]["only_in"], "candidate");
    assert_eq!(report["differences"][0]["candidate"]["value_sats"], 60_000);
}

#[test]
fn oracle_backend_scan_verify_doctor_and_persistent_sync() {
    let (node, oracle) = services();
    let (core_url, oracle_url) = (node.url(), oracle.url());
    let with = |head: &[&'static str], tail: &[&'static str]| -> Vec<String> {
        let mut args: Vec<String> = head.iter().map(|s| s.to_string()).collect();
        args.extend(["--backend", "oracle-v2", "--oracle-url"].map(String::from));
        args.push(oracle_url.clone());
        args.push("--core-rest-url".to_string());
        args.push(core_url.clone());
        args.extend(tail.iter().map(|s| s.to_string()));
        args
    };

    let (code, stdout, stderr) = run(&[
        "doctor",
        "--core-rest-url",
        &core_url,
        "--oracle-url",
        &oracle_url,
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains(
            "backend oracle-v2 : reachable: has the node's tip block (height 8); earlier heights \
             were not probed"
        ),
        "{stdout}"
    );

    let args = with(&["scan"], &["--label", "1", "--json"]);
    let scan = json(&refs(&args), 0);
    assert_eq!(scan["backend_id"], "blindbit_oracle_v2_http");
    assert_eq!(
        scan["detected_received_outputs"].as_array().unwrap().len(),
        8
    );

    // Text output says what kind of evidence every line rests on.
    let args = with(&["scan"], &["--label", "1"]);
    let (code, text, stderr) = run(&refs(&args));
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        text.contains("evidence             : tweak_index: UNVERIFIED index claim")
            && text.contains("index entries seen   : 7")
            && !text.contains("transactions seen"),
        "{text}"
    );
    assert_eq!(
        text.matches("[tweak_index: UNVERIFIED index claim]")
            .count(),
        8,
        "{text}"
    );
    assert!(
        text.contains("NOT checked against the block") && text.contains("not a balance"),
        "{text}"
    );
    let (_, full, _) = run(&[
        "scan",
        "--backend",
        "core",
        "--core-rest-url",
        &core_url,
        "--label",
        "1",
    ]);
    assert!(
        full.contains("evidence             : full_block")
            && full.contains("transactions seen    : 17")
            && full.matches("[full_block]").count() == 7
            && !full.contains("index claim"),
        "{full}"
    );

    let args = with(
        &["verify"],
        &["--reference", "core", "--label", "1", "--json"],
    );
    let report = json(&refs(&args), 4);
    assert_eq!(
        (report["agreed"].as_u64(), report["candidate_only"].as_u64()),
        (Some(7), Some(1))
    );
    assert_eq!(report["reference_backend"], "bitcoin_core_rest");
    assert_eq!(report["unexplained"], 0);

    // Persistent sync through the oracle.
    let dir = std::env::temp_dir().join(format!("ssl-verify-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lab.sqlite3").display().to_string();
    let args = with(
        &["lab", "init"],
        &[
            "--birthday",
            "1",
            "--birthday-basis",
            "fixture start",
            "--label",
            "1",
        ],
    );
    let mut args = refs(&args);
    args.extend(["--db", &db]);
    assert_eq!(run(&args).0, Some(0));
    let args = with(&["sync"], &["--json"]);
    let mut args = refs(&args);
    args.extend(["--db", &db]);
    let report = json(&args, 0);
    assert_eq!(report["caught_up"], true);
    assert_eq!(report["evidence_kind"], "tweak_index");
    assert_eq!(report["new_detections"].as_array().unwrap().len(), 8);
    let (code, listed, _) = run(&["received", "list", "--db", &db]);
    assert_eq!(code, Some(0));
    assert!(
        listed
            .matches("[tweak_index: UNVERIFIED index claim]")
            .count()
            == 8
            && listed.contains("NOT checked against the block"),
        "{listed}"
    );
    let status = json(&["status", "--db", &db, "--json"], 0);
    assert_eq!(status["binding"]["backend_id"], "blindbit_oracle_v2_http");
    let _ = std::fs::remove_dir_all(&dir);

    // An oracle that says "main", or one that is behind the node.
    oracle.set_info("main", 8, false);
    let args = with(&["scan"], &[]);
    let (code, stdout, stderr) = run(&refs(&args));
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("mainnet is refused"),
        "{stderr}"
    );
    oracle.set_info("regtest", 5, false);
    let (code, stdout, stderr) = run(&refs(&args));
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("not fully available"),
        "{stderr}"
    );
    let (code, stdout, _) = run(&[
        "doctor",
        "--core-rest-url",
        &core_url,
        "--oracle-url",
        &oracle_url,
    ]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.contains("backend oracle-v2 : UNAVAILABLE"),
        "{stdout}"
    );
}

#[test]
fn oracle_backend_fails_closed_without_services() {
    let dead = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let (node, _oracle) = services();
    // Node up, oracle down.
    let (code, stdout, stderr) = run(&[
        "scan",
        "--backend",
        "oracle-v2",
        "--oracle-url",
        &dead,
        "--core-rest-url",
        &node.url(),
    ]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("nothing was scanned"),
        "{stderr}"
    );
    // Node down: the header source is required.
    let (code, stdout, stderr) = run(&[
        "scan",
        "--backend",
        "oracle-v2",
        "--oracle-url",
        &dead,
        "--core-rest-url",
        &dead,
    ]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("header source"),
        "{stderr}"
    );
    // Remote oracles are out of scope.
    let (code, _, stderr) = run(&[
        "scan",
        "--backend",
        "oracle-v2",
        "--oracle-url",
        "http://oracle.example.com:8000",
        "--core-rest-url",
        &node.url(),
    ]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("refusing oracle URL"), "{stderr}");
    // verify refuses a reference on another network.
    let (code, _, stderr) = run(&[
        "--network",
        "signet",
        "verify",
        "--backend",
        "local-index",
        "--fixture",
        FIXTURE,
        "--reference",
        "fixture",
    ]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("chain mismatch"), "{stderr}");
}
