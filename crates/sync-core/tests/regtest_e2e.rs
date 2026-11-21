//! Gate 2 — send → mine → detect on a real Bitcoin Core regtest node, through
//! the REST backend.
//!
//! These tests need a Bitcoin Core >= 30 binary and are therefore `#[ignore]`d
//! by default. Run them with:
//!
//! ```sh
//! BITCOIND_EXE=/path/to/bitcoind cargo test -p sync-core --test regtest_e2e -- --ignored
//! ```
//!
//! When run without `BITCOIND_EXE` they fail; they never pass vacuously.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LABEL_NOT_BACKED_UP, LABEL_USED},
    regtest_node::{Node, NodeChain},
    regtest_wallet::{expected_for_labels, standard_scenario, Faucet},
};
use sync_core::{
    engine::{scan_range, ScanError, ScanReport},
    models::EvidenceKind,
    network::{ChainIdentity, LabNetwork, NetworkError},
    protocol::ReceiveScanner,
    source::{bitcoin_core::CoreRestSource, ScanSource, SourceError},
};

const NEEDS_NODE: &str = "needs BITCOIND_EXE (Bitcoin Core >= 30)";

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(LabNetwork::Regtest)
}

fn detected(report: &ScanReport) -> Vec<(u32, sync_core::bitcoin::Txid, u32, u64, Option<u32>)> {
    report
        .detected_received_outputs
        .iter()
        .map(|d| (d.block.height, d.txid, d.vout, d.value_sats, d.label))
        .collect()
}

#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn regtest_send_mine_detect() {
    let _ = NEEDS_NODE;
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let scenario = standard_scenario(&mut faucet);
    assert_eq!(scenario.tip_height, node.block_count());
    assert_eq!(scenario.tip_height, scenario.base_height + 6);

    let source = CoreRestSource::connect(&node.rest_url()).unwrap();
    assert_eq!(source.chain(), regtest());
    let caps = source.capabilities();
    assert!(caps.consensus_validated && !caps.remote);
    assert_eq!(caps.evidence_kind, EvidenceKind::FullBlockWithPrevouts);
    let tip = source.tip().unwrap();
    assert_eq!(tip.height, scenario.tip_height);

    let keys = fixture_gen::lab_receiver_keys();
    let scan = |labels: &[u32], from: u32| {
        let scanner = ReceiveScanner::new(&keys, labels).unwrap();
        scan_range(&source, &scanner, regtest(), from, scenario.tip_height).unwrap()
    };

    // The payment blocks only.
    let first = scenario.base_height + 1;
    let report = scan(&[LABEL_USED], first);
    let want = expected_for_labels(&scenario.expected, &[0, LABEL_USED]);
    assert_eq!(want.len(), 7);
    assert_eq!(
        detected(&report),
        want,
        "expected txid / vout / value / label"
    );
    assert_eq!(report.backend_id, "bitcoin_core_rest");
    assert_eq!(report.blocks_scanned, 6);
    assert_eq!(report.last_block, tip);
    // 6 coinbases + 9 scenario transactions.
    assert_eq!(report.txs_seen, 15);
    assert_eq!(report.txs_scanned, 6);
    assert_eq!(report.txs_skipped["coinbase"], 6);
    assert_eq!(report.txs_skipped["no_eligible_inputs"], 1);
    assert_eq!(report.txs_skipped["no_taproot_outputs"], 1);
    assert_eq!(report.txs_skipped["spends_unknown_segwit_version"], 1);
    assert!(
        report
            .detected_received_outputs
            .iter()
            .all(|d| d.txid != scenario.segwit_v2_txid),
        "the SegWit v2 spending transaction must not be scanned"
    );

    // The whole chain from block 1: coinbase maturity blocks and the node
    // wallet's own funding transactions add nothing.
    let whole = scan(&[LABEL_USED], 1);
    assert_eq!(detected(&whole), want);
    assert_eq!(whole.blocks_scanned, u64::from(scenario.tip_height));
    assert_eq!(
        whole.first_block.prev_hash,
        LabNetwork::Regtest.genesis_hash()
    );

    // Labels decide what a restored wallet sees, exactly as on the fixture.
    assert_eq!(scan(&[], first).detected_received_outputs.len(), 5);
    assert_eq!(
        detected(&scan(&[LABEL_USED, LABEL_NOT_BACKED_UP], first)),
        expected_for_labels(&scenario.expected, &[0, LABEL_USED, LABEL_NOT_BACKED_UP])
    );
    assert_eq!(
        scan(&[LABEL_USED, LABEL_NOT_BACKED_UP], first)
            .detected_received_outputs
            .len(),
        8
    );

    // Someone else's keys see only the payment made to them.
    let other = ReceiveScanner::new(&fixture_gen::other_receiver_keys(), &[LABEL_USED]).unwrap();
    let theirs = scan_range(&source, &other, regtest(), first, scenario.tip_height).unwrap();
    assert_eq!(theirs.detected_received_outputs.len(), 1);
    assert_eq!(theirs.detected_received_outputs[0].value_sats, 90_000);

    // Transfer counters are real.
    let stats = source.stats();
    assert!(
        stats.requests > 0 && stats.downloaded_bytes > 0,
        "{stats:?}"
    );

    // Fail-closed range handling.
    let scanner = ReceiveScanner::new(&keys, &[]).unwrap();
    for (from, to) in [(0, 3), (1, scenario.tip_height + 1)] {
        assert!(
            matches!(
                scan_range(&source, &scanner, regtest(), from, to),
                Err(ScanError::Source(SourceError::RangeUnavailable { .. }))
            ),
            "{from}..={to}"
        );
    }
    // A wallet configured for signet refuses a regtest node.
    assert!(matches!(
        scan_range(
            &source,
            &scanner,
            ChainIdentity::for_network(LabNetwork::Signet),
            first,
            scenario.tip_height
        ),
        Err(ScanError::ChainMismatch { .. })
    ));
}

/// T12: a node that goes away must produce an error, never a "scanned" report.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn stopped_node_is_an_error_not_a_completed_scan() {
    let mut node = Node::regtest();
    let address = {
        node.cli(&["createwallet", "w"]);
        node.cli(&["getnewaddress", "", "bech32"])
    };
    node.generate_to(5, &address);
    let source = CoreRestSource::connect(&node.rest_url()).unwrap();
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();
    assert_eq!(
        scan_range(&source, &scanner, regtest(), 1, 5)
            .unwrap()
            .blocks_scanned,
        5
    );

    let url = node.rest_url();
    node.stop();
    assert!(matches!(
        scan_range(&source, &scanner, regtest(), 1, 5),
        Err(ScanError::Source(SourceError::Unavailable(_)))
    ));
    assert!(matches!(source.tip(), Err(SourceError::Unavailable(_))));
    assert!(matches!(
        CoreRestSource::connect(&url),
        Err(SourceError::Unavailable(_))
    ));
}

/// The backend refuses a node that reports mainnet. The node here has mainnet
/// parameters but no network access whatsoever and holds only the genesis block.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn mainnet_node_is_refused() {
    let node = Node::start(NodeChain::OfflineMainnet);
    let info = node.cli_json(&["getblockchaininfo"]);
    assert_eq!(info["chain"], "main");
    assert_eq!(node.cli_json(&["getnetworkinfo"])["connections"], 0);
    assert_eq!(
        CoreRestSource::connect(&node.rest_url()).err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
}
