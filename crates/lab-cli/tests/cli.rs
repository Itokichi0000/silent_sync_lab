//! End-to-end tests of the `silent-sync` binary.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_silent-sync");
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/local/regtest_basic_v1.json"
);
const BANNER: &str = "EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS";
/// Private keys of the lab receiver as published in the BIP352 vectors. They
/// are public test material, but the CLI must still never print them.
const SCAN_PRIVATE_KEY: &str = "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c";
const SPEND_PRIVATE_KEY: &str = "9d6ad855ce3417ef84e836892e5a56392bfba05fa5d97ccea30e266f540e08b3";

fn run(args: &[&str]) -> (Output, String, String) {
    let output = Command::new(BIN).args(args).output().unwrap();
    let stdout = String::from_utf8(output.stdout.clone()).unwrap();
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    for text in [&stdout, &stderr] {
        let lower = text.to_lowercase();
        assert!(
            !lower.contains(SCAN_PRIVATE_KEY),
            "scan private key printed"
        );
        assert!(
            !lower.contains(SPEND_PRIVATE_KEY),
            "spend private key printed"
        );
    }
    assert!(
        stderr.starts_with(BANNER),
        "banner must be the first thing printed: {stderr}"
    );
    (output, stdout, stderr)
}

fn sync_args<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["scan", "--backend", "fixture", "--fixture", FIXTURE];
    args.extend_from_slice(extra);
    args
}

#[test]
fn mainnet_is_refused_by_every_command() {
    for name in ["mainnet", "main", "bitcoin", "MAINNET"] {
        let commands: [Vec<&str>; 4] = [
            vec!["--network", name, "doctor"],
            vec!["--network", name, "address", "show"],
            vec!["address", "show", "--network", name],
            sync_args(&["--network", name]),
        ];
        for args in commands {
            let (output, stdout, stderr) = run(&args);
            assert!(!output.status.success(), "{args:?} must fail");
            assert!(stderr.contains("mainnet is refused"), "{args:?}: {stderr}");
            assert!(stdout.is_empty(), "{args:?} printed to stdout: {stdout}");
        }
    }
}

#[test]
fn unsupported_networks_are_rejected() {
    let (output, _, stderr) = run(&["--network", "testnet", "doctor"]);
    assert!(!output.status.success());
    assert!(stderr.contains("unsupported network"), "{stderr}");
}

#[test]
fn address_show_uses_test_network_prefixes_only() {
    let (output, stdout, _) = run(&["address", "show"]);
    assert!(output.status.success());
    assert!(stdout.starts_with("sprt1"), "{stdout}");

    let (output, signet, _) = run(&["--network", "signet", "address", "show"]);
    assert!(output.status.success());
    assert!(signet.starts_with("tsp1"), "{signet}");

    let (output, labelled, stderr) =
        run(&["--network", "signet", "address", "show", "--label", "1"]);
    assert!(output.status.success());
    assert!(labelled.starts_with("tsp1") && labelled != signet);
    assert!(stderr.contains("must be backed up"), "{stderr}");

    for address in [&stdout, &signet, &labelled] {
        assert!(
            !address.starts_with("sp1"),
            "mainnet prefix printed: {address}"
        );
        assert_eq!(address.lines().count(), 1);
    }
}

#[test]
fn scan_on_fixture_reports_the_expected_outputs_as_json() {
    let (output, stdout, stderr) = run(&sync_args(&["--label", "1", "--json"]));
    assert!(output.status.success(), "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(report["backend_id"], "fixture");
    assert_eq!(report["chain"]["network"], "regtest");
    assert_eq!(report["blocks_scanned"], 8);
    assert_eq!(report["labels_scanned"], serde_json::json!([0, 1]));
    assert_eq!(report["txs_skipped"]["spends_unknown_segwit_version"], 1);

    let detected: Vec<(u64, String, u64, u64, Option<u64>)> = report["detected_received_outputs"]
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
        .collect();
    let tx = |s: &str| s.to_string();
    assert_eq!(
        detected,
        vec![
            (
                2,
                tx("391d63ac72c9c60e7cb066ad829aadd8cf201e4fff3ad07589d7f705cde04b11"),
                0,
                50_000,
                None
            ),
            (
                3,
                tx("932699c16df7f7d09801d6ba104af2a2a0358eb53b71449ed6810ac449f395a2"),
                1,
                10_000,
                None
            ),
            (
                3,
                tx("932699c16df7f7d09801d6ba104af2a2a0358eb53b71449ed6810ac449f395a2"),
                2,
                20_000,
                None
            ),
            (
                5,
                tx("f9accc13beb1f55d035b11b097ce8c6b5701e9061c6720ea046de93af817a72d"),
                0,
                7_000,
                Some(1)
            ),
            (
                5,
                tx("f9accc13beb1f55d035b11b097ce8c6b5701e9061c6720ea046de93af817a72d"),
                1,
                3_000,
                Some(0)
            ),
            (
                7,
                tx("defc7dcf7f5465f1a54710582fea2cfddb782bb72ae5a2cde98c883b24160a5d"),
                1,
                15_000,
                Some(0)
            ),
            (
                8,
                tx("bfede6f09b7d080f72b553ee2b73f3537ba0c76a57929c3a739baf1bbed61bc8"),
                0,
                1,
                None
            ),
        ]
    );
    // No field of the export is named like key material.
    for forbidden in ["tweak", "scan_sk", "priv", "secret", "seed"] {
        assert!(!stdout.to_lowercase().contains(forbidden), "{forbidden}");
    }
}

#[test]
fn scan_text_output_does_not_claim_a_balance() {
    let (output, stdout, stderr) = run(&sync_args(&["--from", "2", "--to", "3"]));
    assert!(output.status.success(), "{stderr}");
    assert!(stdout.contains("detected_received_outputs: 3"), "{stdout}");
    assert!(stdout.contains("not a balance"));
    assert!(stderr.contains("stateless"));
    assert!(!stdout.to_lowercase().contains("total"));
}

#[test]
fn scan_fails_closed() {
    // Range beyond the fixture: error, nothing reported.
    let (output, stdout, stderr) = run(&sync_args(&["--to", "99"]));
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.is_empty());
    assert!(stderr.contains("not fully available") && stderr.contains("scan aborted"));

    // Wallet on signet, fixture on regtest.
    let (output, stdout, stderr) = run(&sync_args(&["--network", "signet"]));
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("chain mismatch: --network is signet but source `fixture` serves regtest")
            && stderr.contains("nothing was scanned"),
        "{stderr}"
    );

    // Missing / unreadable fixture.
    let (output, stdout, _) = run(&["scan", "--backend", "fixture"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.is_empty());
    let (output, stdout, _) = run(&[
        "scan",
        "--backend",
        "fixture",
        "--fixture",
        "/nonexistent.json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.is_empty());
}

#[test]
fn doctor_without_urls_probes_nothing_and_says_so() {
    let (output, stdout, _) = run(&["doctor"]);
    assert!(output.status.success());
    assert!(stdout.contains("backend core      : not probed"));
    assert!(stdout.contains("backend oracle-v2 : not probed"));
    assert!(stdout.contains("backend local-index: available"));
    assert!(stdout.contains("persistence       : SQLite store"));
}

/// A loopback port nothing listens on.
fn dead_port_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    url
}

/// Without a node, the Core backend must fail visibly and scan nothing.
#[test]
fn core_backend_without_a_node_fails_closed() {
    let url = dead_port_url();
    let (output, stdout, stderr) = run(&["scan", "--backend", "core", "--core-rest-url", &url]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("source is unavailable") && stderr.contains("nothing was scanned"),
        "{stderr}"
    );

    // `doctor` reports it and does not exit successfully.
    let (output, stdout, _) = run(&["doctor", "--core-rest-url", &url]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout.contains("backend core      : UNAVAILABLE"),
        "{stdout}"
    );

    // Remote, named or non-HTTP nodes are refused before any request, and a
    // password in the URL is not echoed.
    for url in [
        "http://192.0.2.1:8332",
        "https://127.0.0.1:18443",
        "http://example.com:8332",
        "http://localhost:18443",
        "http://127.0.0.1",
        "http://rpcuser:hunter2@127.0.0.1:18443",
    ] {
        let (output, stdout, stderr) = run(&["scan", "--backend", "core", "--core-rest-url", url]);
        assert_eq!(output.status.code(), Some(1), "{url}");
        assert!(stdout.is_empty());
        assert!(stderr.contains("refusing REST URL"), "{url}: {stderr}");
        assert!(!stderr.contains("hunter2"), "{stderr}");
    }
}

/// The Core backend through the CLI, against an in-process fake node serving
/// the fixture: same result as scanning the fixture file.
#[test]
fn core_backend_through_the_cli_matches_the_fixture_scan() {
    let fixture: silent_sync::source::fixture::FixtureFile =
        serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    let node = lab_testkit::fake_http::FakeCoreRest::serve(&fixture);
    let url = node.url();

    let (output, stdout, stderr) = run(&["doctor", "--core-rest-url", &url]);
    assert!(output.status.success(), "{stderr}");
    assert!(
        stdout.contains("backend core      : reachable: regtest at height 8"),
        "{stdout}"
    );
    let (output, stdout, _) = run(&["--network", "signet", "doctor", "--core-rest-url", &url]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout.contains("UNUSABLE: the node is on regtest, but --network is signet"),
        "{stdout}"
    );

    let detections = |args: &[&str]| -> serde_json::Value {
        let (output, stdout, stderr) = run(args);
        assert!(output.status.success(), "{args:?}: {stderr}");
        let mut report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        let mut list = report["detected_received_outputs"].take();
        for d in list.as_array_mut().unwrap() {
            d.as_object_mut().unwrap().remove("provenance");
        }
        list
    };
    let via_core = detections(&[
        "scan",
        "--backend",
        "core",
        "--core-rest-url",
        &url,
        "--label",
        "1",
        "--json",
    ]);
    let via_file = detections(&sync_args(&["--label", "1", "--json"]));
    assert_eq!(via_core.as_array().unwrap().len(), 7);
    assert_eq!(via_core, via_file);

    // A node that says "main" is refused through the CLI as well.
    node.set_chain_info("main", 8, node.hashes[8]);
    let (output, stdout, stderr) = run(&["scan", "--backend", "core", "--core-rest-url", &url]);
    assert_eq!(output.status.code(), Some(1));
    // (Worded as a genesis mismatch when another crate in the build turned
    // on `silent-sync`'s `mainnet` feature: "main" is then a network, and
    // the chain served is not that network's.)
    assert!(
        stdout.is_empty()
            && (stderr.contains("mainnet is refused")
                || stderr.contains("genesis hash mismatch for mainnet"))
            && stderr.contains("nothing was scanned"),
        "{stderr}"
    );
}

#[test]
fn there_is_no_send_or_balance_command() {
    for command in ["send", "sign", "balance", "spend", "pay"] {
        let (output, _, stderr) = run(&[command]);
        assert_eq!(output.status.code(), Some(2), "{command}");
        assert!(stderr.contains("unrecognized subcommand"), "{stderr}");
    }
}

/// When stdout cannot be written (full disk, reader gone as in
/// `... | head -c 0`), the command must fail visibly: no panic, no success.
/// `/dev/full` makes every write fail deterministically.
#[cfg(target_os = "linux")]
#[test]
fn unwritable_stdout_is_an_error_not_a_panic() {
    for args in [
        sync_args(&["--label", "1", "--json"]),
        sync_args(&[]),
        vec!["doctor"],
        vec!["address", "show"],
    ] {
        let full = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        let output = Command::new(BIN)
            .args(&args)
            .stdout(std::process::Stdio::from(full))
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(
            stderr.contains("cannot write to stdout"),
            "{args:?}: {stderr}"
        );
        assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
    }
}
