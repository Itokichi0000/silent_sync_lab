//! End-to-end: the `silent-sync` binary against a real regtest node.
//! Ignored by default; needs `BITCOIND_EXE` (see `scripts/fetch_bitcoind.sh`).
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::LABEL_USED,
    regtest_node::{Node, NodeChain},
    regtest_wallet::{expected_for_labels, standard_scenario, Faucet},
};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_silent-sync");

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let output = Command::new(BIN).args(args).output().unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn sync_against_a_regtest_node_reports_the_expected_outputs() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let scenario = standard_scenario(&mut faucet);
    let url = node.rest_url();

    let (code, stdout, stderr) = run(&["doctor", "--core-rest-url", &url]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains(&format!(
            "reachable at {url}: regtest at height {}",
            scenario.tip_height
        )),
        "{stdout}"
    );

    let from = (scenario.base_height + 1).to_string();
    let label = LABEL_USED.to_string();
    let (code, stdout, stderr) = run(&[
        "sync",
        "--backend",
        "core",
        "--core-rest-url",
        &url,
        "--from",
        &from,
        "--label",
        &label,
        "--json",
    ]);
    assert_eq!(code, Some(0), "{stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["backend_id"], "bitcoin_core_rest");
    assert_eq!(report["chain"]["network"], "regtest");
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
    let want: Vec<_> = expected_for_labels(&scenario.expected, &[0, LABEL_USED])
        .into_iter()
        .map(|(h, txid, vout, value, label)| {
            (
                u64::from(h),
                txid.to_string(),
                u64::from(vout),
                value,
                label.map(u64::from),
            )
        })
        .collect();
    assert_eq!(detected, want);

    // The wallet is told signet, the node is regtest: refuse.
    let (code, stdout, stderr) = run(&[
        "--network",
        "signet",
        "sync",
        "--backend",
        "core",
        "--core-rest-url",
        &url,
    ]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("chain mismatch"),
        "{stderr}"
    );
    let (_, stdout, _) = run(&["--network", "signet", "doctor", "--core-rest-url", &url]);
    assert!(stdout.contains("UNUSABLE: node at"), "{stdout}");
}

#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn cli_refuses_a_mainnet_node() {
    let node = Node::start(NodeChain::OfflineMainnet);
    let url = node.rest_url();
    let (code, stdout, stderr) = run(&["sync", "--backend", "core", "--core-rest-url", &url]);
    assert_eq!(code, Some(1));
    assert!(stdout.is_empty());
    assert!(
        stderr.contains("mainnet is refused") && stderr.contains("nothing was scanned"),
        "{stderr}"
    );
    let (code, stdout, _) = run(&["doctor", "--core-rest-url", &url]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("UNAVAILABLE: mainnet is refused"),
        "{stdout}"
    );
}
