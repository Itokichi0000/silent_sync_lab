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
fn scan_against_a_regtest_node_reports_the_expected_outputs() {
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
        "scan",
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
        "scan",
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
    let (code, stdout, stderr) = run(&["scan", "--backend", "core", "--core-rest-url", &url]);
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

/// Gate 3 on a real node: persistent sync, a real reorganization made with
/// `invalidateblock`, and re-detection once the orphaned payments confirm again.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn persistent_sync_follows_a_real_reorg() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let scenario = standard_scenario(&mut faucet);
    let url = node.rest_url();
    let tip = scenario.tip_height;
    let dir = std::env::temp_dir().join(format!("ssl-regtest-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("lab.sqlite3").display().to_string();
    let birthday = (scenario.base_height + 1).to_string();

    let run_ok = |args: &[&str]| {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, Some(0), "{args:?}: {stderr}");
        stdout
    };
    let sync_json = || -> serde_json::Value {
        serde_json::from_str(&run_ok(&[
            "sync",
            "--db",
            &db,
            "--backend",
            "core",
            "--core-rest-url",
            &url,
            "--chunk-size",
            "2",
            "--json",
        ]))
        .unwrap()
    };
    let received = || -> Vec<(u64, String, u64, u64)> {
        let list: serde_json::Value =
            serde_json::from_str(&run_ok(&["received", "list", "--db", &db, "--json"])).unwrap();
        list.as_array()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["block"]["height"].as_u64().unwrap(),
                    d["txid"].as_str().unwrap().to_string(),
                    d["vout"].as_u64().unwrap(),
                    d["value_sats"].as_u64().unwrap(),
                )
            })
            .collect()
    };

    run_ok(&[
        "lab",
        "init",
        "--db",
        &db,
        "--backend",
        "core",
        "--core-rest-url",
        &url,
        "--birthday",
        &birthday,
        "--birthday-basis",
        "first block after the test coins were funded",
        "--label",
        &LABEL_USED.to_string(),
    ]);
    let first = sync_json();
    assert_eq!(first["caught_up"], true);
    assert_eq!(first["blocks_scanned"], 6);
    let want: Vec<(u64, String, u64, u64)> =
        expected_for_labels(&scenario.expected, &[0, LABEL_USED])
            .into_iter()
            .map(|(h, txid, vout, value, _)| {
                (u64::from(h), txid.to_string(), u64::from(vout), value)
            })
            .collect();
    assert_eq!(received(), want);

    // Disconnect the last two blocks. The node is now *behind* our checkpoint
    // on the same chain: that alone must not make us forget anything.
    let orphaned = node.cli(&["getblockhash", &(tip - 1).to_string()]);
    node.cli(&["invalidateblock", &orphaned]);
    assert_eq!(node.block_count(), tip - 2);
    let (code, stdout, stderr) = run(&[
        "sync",
        "--db",
        &db,
        "--backend",
        "core",
        "--core-rest-url",
        &url,
    ]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("is behind the stored checkpoint"),
        "{stderr}"
    );
    assert_eq!(received(), want);

    // Three empty blocks on top: a longer competing chain without the payments.
    for _ in 0..3 {
        node.generate_block_with(faucet.miner_address(), &[]);
    }
    assert_eq!(node.block_count(), tip + 1);
    let reorg = sync_json();
    assert_eq!(reorg["reorg"]["fork_height"], tip - 2);
    assert_eq!(reorg["reorg"]["blocks_rolled_back"], 2);
    assert_eq!(
        reorg["reorg"]["detections_removed"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(reorg["blocks_scanned"], 3);
    assert_eq!(reorg["new_detections"].as_array().unwrap().len(), 0);
    assert_eq!(reorg["caught_up"], true);
    assert_eq!(
        received(),
        want[..5].to_vec(),
        "detections in orphaned blocks are removed"
    );

    // The two orphaned payments confirm again, in one new block.
    let raw: Vec<String> = scenario
        .last_two_payments
        .iter()
        .map(sync_core::bitcoin::consensus::encode::serialize_hex)
        .collect();
    node.generate_block_with(faucet.miner_address(), &raw);
    let again = sync_json();
    assert!(again["reorg"].is_null());
    assert_eq!(again["new_detections"].as_array().unwrap().len(), 2);
    let now = received();
    assert_eq!(now.len(), 7);
    assert_eq!(now[..5], want[..5]);
    assert!(
        now[5..].iter().all(|d| d.0 == u64::from(tip) + 2),
        "both payments are confirmed in the new block"
    );
    // Same outputs as before the reorg. Both now sit at the same height, where
    // the store orders by txid, so compare without relying on order.
    let outputs = |list: &[(u64, String, u64, u64)]| {
        let mut ids: Vec<(String, u64, u64)> =
            list.iter().map(|d| (d.1.clone(), d.2, d.3)).collect();
        ids.sort();
        ids
    };
    assert_eq!(outputs(&now[5..]), outputs(&want[5..]));
    let _ = std::fs::remove_dir_all(&dir);
}
