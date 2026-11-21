//! Gate 4 on real software: BlindBit Oracle v2 indexing a real Bitcoin Core
//! regtest node, compared with scanning that node directly.
//!
//! Ignored by default; needs both binaries:
//!
//! ```sh
//! export BITCOIND_EXE="$(scripts/fetch_bitcoind.sh /somewhere)"
//! export BLINDBIT_ORACLE_EXE="$(scripts/build_blindbit_oracle.sh /somewhere-else)"
//! cargo test -p silent-sync --test oracle_e2e -- --ignored
//! ```
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LABEL_NOT_BACKED_UP, LABEL_USED},
    oracle_node::Oracle,
    regtest_node::Node,
    regtest_wallet::{expected_for_labels, standard_scenario, Faucet},
};
use silent_sync::{
    engine::scan_range,
    network::{ChainIdentity, Network},
    protocol::ReceiveScanner,
    source::{bitcoin_core::CoreRestSource, blindbit::BlindbitHttpSource, ScanSource, SourceError},
    store::{MemoryStore, SyncStore},
    sync::{new_store_meta, sync, SyncOptions},
    verify::{cross_check, Explanation, Side},
};

#[test]
#[ignore = "needs BITCOIND_EXE and BLINDBIT_ORACLE_EXE"]
fn real_oracle_matches_core_except_for_the_segwit_version_rule() {
    let regtest = ChainIdentity::for_network(Network::Regtest);
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let scenario = standard_scenario(&mut faucet);
    let oracle = Oracle::start(&node);
    let (first, tip) = (scenario.base_height + 1, scenario.tip_height);

    let core = CoreRestSource::connect(&node.rest_url()).unwrap();
    let via_oracle = BlindbitHttpSource::connect(
        &oracle.url(),
        CoreRestSource::connect(&node.rest_url()).unwrap(),
    )
    .unwrap();
    let keys = fixture_gen::lab_receiver_keys();

    for labels in [
        vec![LABEL_USED],
        vec![],
        vec![LABEL_USED, LABEL_NOT_BACKED_UP],
    ] {
        let scanner = ReceiveScanner::new(&keys, &labels).unwrap();
        let mut known = labels.clone();
        known.push(0);
        let want = expected_for_labels(&scenario.expected, &known);

        // Core directly: exactly what the sender created for these labels.
        let direct = scan_range(&core, &scanner, regtest, first, tip).unwrap();
        let direct: Vec<_> = direct
            .detected_received_outputs
            .iter()
            .map(|d| (d.block.height, d.txid, d.vout, d.value_sats, d.label))
            .collect();
        assert_eq!(direct, want, "labels {labels:?}");

        // Through the oracle: the same, plus the output of the transaction
        // that spends a SegWit v2 output, which BIP352 says not to scan.
        let indexed = scan_range(&via_oracle, &scanner, regtest, first, tip).unwrap();
        let mut indexed: Vec<_> = indexed
            .detected_received_outputs
            .iter()
            .map(|d| (d.block.height, d.txid, d.vout, d.value_sats, d.label))
            .collect();
        let extra: Vec<_> = indexed
            .iter()
            .filter(|d| d.1 == scenario.segwit_v2_txid)
            .cloned()
            .collect();
        assert_eq!(extra.len(), 1, "labels {labels:?}");
        assert_eq!((extra[0].2, extra[0].3, extra[0].4), (0, 60_000, None));
        indexed.retain(|d| d.1 != scenario.segwit_v2_txid);
        assert_eq!(indexed, want, "labels {labels:?}");

        // The cross-check says exactly that.
        let check = cross_check(&via_oracle, &core, &scanner, regtest, first, tip).unwrap();
        assert_eq!(check.agreed as usize, want.len());
        assert_eq!(
            (
                check.candidate_only,
                check.reference_only,
                check.unexplained
            ),
            (1, 0, 0)
        );
        assert!(check.block_hash_mismatches.is_empty());
        let diff = &check.differences[0];
        assert_eq!(
            (diff.txid, diff.only_in),
            (scenario.segwit_v2_txid, Some(Side::Candidate))
        );
        assert_eq!(
            diff.explanation,
            Explanation::ReferenceNotEligible {
                reason: "spends_unknown_segwit_version"
            }
        );
    }

    // The whole chain from height 1, including the coinbase-maturity blocks
    // and the node wallet's own funding transactions.
    let scanner = ReceiveScanner::new(&keys, &[LABEL_USED]).unwrap();
    let whole = cross_check(&via_oracle, &core, &scanner, regtest, 1, tip).unwrap();
    assert_eq!(
        (whole.agreed, whole.candidate_only, whole.reference_only),
        (7, 1, 0)
    );

    // Persistent sync through the oracle, then new blocks the oracle has not
    // indexed yet: an error or a later success, never silently "caught up".
    let mut store = MemoryStore::new();
    store
        .initialize(&new_store_meta(&via_oracle, &scanner, first, "test"))
        .unwrap();
    let report = sync(&via_oracle, &scanner, &mut store, &SyncOptions::default()).unwrap();
    assert!(report.caught_up);
    assert_eq!(store.detections().unwrap().len(), 8);

    node.generate_to(3, faucet.miner_address());
    match sync(&via_oracle, &scanner, &mut store, &SyncOptions::default()) {
        Ok(report) => assert!(report.caught_up && report.blocks_scanned == 3),
        Err(e) => {
            assert!(
                matches!(
                    e,
                    silent_sync::sync::SyncError::Scan(silent_sync::engine::ScanError::Source(
                        SourceError::RangeUnavailable { .. } | SourceError::InvalidData(_)
                    ))
                ),
                "{e}"
            );
            oracle.wait_for_height(tip + 3);
            let report = sync(&via_oracle, &scanner, &mut store, &SyncOptions::default()).unwrap();
            assert!(report.caught_up);
        }
    }
    assert_eq!(store.checkpoint().unwrap().unwrap().height, tip + 3);
    assert_eq!(store.detections().unwrap().len(), 8);
    assert_eq!(via_oracle.tip().unwrap().height, tip + 3);
}
