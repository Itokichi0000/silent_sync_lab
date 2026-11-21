//! The Bitcoin Core REST backend against an in-process fake node serving the
//! fixture chain. No bitcoind needed, so these run in the normal test suite.
//!
//! Two purposes: the backend must give the same result as the fixture adapter
//! on the same chain, and it must refuse a node whose answers do not add up.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_core_rest::{encode_spent_outputs, FakeCoreRest},
    fixture_gen::{self, LABEL_USED},
};
use sync_core::{
    bitcoin::{
        blockdata::constants::genesis_block,
        consensus::encode::{deserialize, serialize},
        Block, Network, Witness,
    },
    engine::{scan_range, ScanError},
    network::{ChainIdentity, LabNetwork, NetworkError},
    protocol::ReceiveScanner,
    source::{
        bitcoin_core::CoreRestSource,
        fixture::{FixtureFile, FixtureSource},
        ScanSource, SourceError,
    },
};

const FIXTURE_JSON: &str = include_str!("../../../fixtures/local/regtest_basic_v1.json");
const TIP: u32 = 8;

fn fixture() -> FixtureFile {
    serde_json::from_str(FIXTURE_JSON).unwrap()
}

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(LabNetwork::Regtest)
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap()
}

/// Scans the whole fake chain; the error is rendered for matching.
fn scan(node: &FakeCoreRest) -> Result<usize, String> {
    let source = CoreRestSource::connect(&node.url()).map_err(|e| e.to_string())?;
    scan_range(&source, &scanner(), regtest(), 1, TIP)
        .map(|r| r.detected_received_outputs.len())
        .map_err(|e| e.to_string())
}

fn assert_refused(result: Result<usize, String>, needle: &str) {
    match result {
        Err(e) => assert!(e.contains(needle), "`{e}` should mention `{needle}`"),
        Ok(n) => panic!("expected a refusal mentioning `{needle}`, got {n} detections"),
    }
}

#[test]
fn honest_node_gives_the_same_result_as_the_fixture_adapter() {
    let node = FakeCoreRest::serve(&fixture());
    let core = CoreRestSource::connect(&node.url()).unwrap();
    let file = FixtureSource::from_json_str(FIXTURE_JSON).unwrap();
    assert_eq!(core.chain(), file.chain());
    assert_eq!(core.tip().unwrap(), file.tip().unwrap());

    let via_core = scan_range(&core, &scanner(), regtest(), 1, TIP).unwrap();
    let via_file = scan_range(&file, &scanner(), regtest(), 1, TIP).unwrap();
    assert_eq!(via_core.detected_received_outputs.len(), 7);
    // Identical apart from which backend the evidence came from.
    let strip = |r: &sync_core::engine::ScanReport| {
        r.detected_received_outputs
            .iter()
            .map(|d| (d.block, d.txid, d.vout, d.value_sats, d.label))
            .collect::<Vec<_>>()
    };
    assert_eq!(strip(&via_core), strip(&via_file));
    assert_eq!(via_core.txs_skipped, via_file.txs_skipped);
    assert_eq!(
        (via_core.first_block, via_core.last_block),
        (via_file.first_block, via_file.last_block)
    );
    assert_eq!(via_core.backend_id, "bitcoin_core_rest");

    // Sub-range, and the raw batches are identical too.
    assert_eq!(
        core.fetch_range(3, 5).unwrap(),
        file.fetch_range(3, 5).unwrap()
    );

    let stats = core.stats();
    // connect: 2; tip: 2; two fetches: 1 + 3 per block each.
    assert_eq!(stats.requests, 2 + 2 + (1 + 3 * 8) + (1 + 3 * 3));
    assert!(stats.downloaded_bytes > 3_000);
}

#[test]
fn mainnet_and_foreign_chains_are_refused() {
    let node = FakeCoreRest::serve(&fixture());
    let best = node.hashes[TIP as usize];

    node.set_chain_info("main", TIP, best);
    assert_eq!(
        CoreRestSource::connect(&node.url()).err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
    for chain in ["test", "testnet4", "MAIN"] {
        node.set_chain_info(chain, TIP, best);
        assert!(matches!(
            CoreRestSource::connect(&node.url()),
            Err(SourceError::Network(NetworkError::Unsupported(_)))
        ));
    }

    // Says "regtest" but its block 0 is the mainnet genesis block.
    node.set_chain_info("regtest", TIP, best);
    let mainnet_genesis = genesis_block(Network::Bitcoin).block_hash();
    node.set(
        "/rest/blockhashbyheight/0.hex",
        format!("{mainnet_genesis}\n").into_bytes(),
    );
    assert!(matches!(
        CoreRestSource::connect(&node.url()),
        Err(SourceError::Network(NetworkError::GenesisMismatch { .. }))
    ));

    // Says "signet" while serving the regtest chain.
    let node = FakeCoreRest::serve(&fixture());
    node.set_chain_info("signet", TIP, best);
    assert!(matches!(
        CoreRestSource::connect(&node.url()),
        Err(SourceError::Network(NetworkError::GenesisMismatch { .. }))
    ));

    // The node changes its mind after we connected.
    let node = FakeCoreRest::serve(&fixture());
    let source = CoreRestSource::connect(&node.url()).unwrap();
    node.set_chain_info("main", TIP, best);
    assert_eq!(
        source.tip().err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
}

/// A block served without witnesses (as a node returns for a non-witness
/// request) hashes the same and must still be refused.
#[test]
fn witness_stripped_blocks_from_the_node_are_refused() {
    let node = FakeCoreRest::serve(&fixture());
    let path = node.block_path(2);
    let mut block: Block = deserialize(&node.get(&path)).unwrap();
    let hash = block.block_hash();
    for tx in &mut block.txdata {
        for txin in &mut tx.input {
            txin.witness = Witness::new();
        }
    }
    assert_eq!(
        block.block_hash(),
        hash,
        "stripping does not change the hash"
    );
    node.set(&path, serialize(&block));
    assert_refused(scan(&node), "block 2: witness commitment");

    // Only the transactions stripped, coinbase intact.
    let node = FakeCoreRest::serve(&fixture());
    let mut block: Block = deserialize(&node.get(&path)).unwrap();
    block.txdata[1].input[0].witness = Witness::new();
    node.set(&path, serialize(&block));
    assert_refused(scan(&node), "block 2: witness commitment");
}

#[test]
fn inconsistent_responses_are_refused() {
    let fresh = || FakeCoreRest::serve(&fixture());

    // Sanity: an untouched node scans fine.
    assert_eq!(scan(&fresh()), Ok(7));

    // A different block than the one asked for.
    let node = fresh();
    node.set(&node.block_path(3), node.get(&node.block_path(4)));
    assert_refused(scan(&node), "block 3: prev_blockhash");
    let node = fresh();
    node.set(&node.block_path(1), node.get(&node.block_path(2)));
    assert_refused(scan(&node), "block 1: block hash is");

    // A block whose transactions were altered.
    let node = fresh();
    let mut block: Block = deserialize(&node.get(&node.block_path(2))).unwrap();
    block.txdata.pop();
    node.set(&node.block_path(2), serialize(&block));
    assert_refused(
        scan(&node),
        "block 2: transactions do not match the header merkle root",
    );

    // Garbage and trailing bytes.
    let node = fresh();
    node.set(&node.block_path(2), vec![0u8; 10]);
    assert_refused(scan(&node), "block 2: cannot decode block");
    let node = fresh();
    let mut body = node.get(&node.block_path(2));
    body.push(0);
    node.set(&node.block_path(2), body);
    assert_refused(scan(&node), "block 2: 1 trailing bytes after block");

    // Spent outputs of another block, truncated, with a trailing byte, or
    // with one prevout missing.
    let node = fresh();
    node.set(&node.spent_path(3), node.get(&node.spent_path(2)));
    assert_refused(scan(&node), "block 3: 3 transactions but 2 prevout lists");
    let node = fresh();
    let mut body = node.get(&node.spent_path(2));
    body.pop();
    node.set(&node.spent_path(2), body);
    assert_refused(scan(&node), "block 2: spent outputs:");
    let node = fresh();
    let mut body = node.get(&node.spent_path(2));
    body.push(0);
    node.set(&node.spent_path(2), body);
    assert_refused(scan(&node), "block 2: spent outputs: 1 trailing bytes");
    let node = fresh();
    node.set(
        &node.spent_path(3),
        encode_spent_outputs(&[vec![], vec![], vec![]]),
    );
    assert_refused(scan(&node), "inputs needing prevouts but 0 were supplied");

    // The height index points at a block of another height (as during a reorg).
    let node = fresh();
    node.set(
        "/rest/blockhashbyheight/5.hex",
        format!("{}\n", node.hashes[6]).into_bytes(),
    );
    assert_refused(scan(&node), "block 5:");
    let node = fresh();
    node.set("/rest/blockhashbyheight/5.hex", b"not a hash".to_vec());
    assert_refused(scan(&node), "blockhashbyheight/5: not a block hash");

    // Missing data, HTTP errors.
    let node = fresh();
    node.remove(&node.spent_path(4));
    assert_refused(scan(&node), "HTTP 404");
    let node = fresh();
    node.remove(&node.block_path(8));
    assert_refused(scan(&node), "HTTP 404");
}

#[test]
fn ranges_outside_the_node_s_chain_are_refused() {
    let node = FakeCoreRest::serve(&fixture());
    let source = CoreRestSource::connect(&node.url()).unwrap();
    for (from, to) in [(0, 3), (1, TIP + 1), (TIP + 1, TIP + 2)] {
        assert_eq!(
            source.fetch_range(from, to),
            Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: 1,
                available_to: TIP
            })
        );
    }
    assert_eq!(
        source.fetch_range(4, 3),
        Err(SourceError::InvalidRange { from: 4, to: 3 })
    );

    // The node reports a shorter chain than before: the tail is unavailable,
    // not silently skipped.
    node.set_chain_info("regtest", 5, node.hashes[5]);
    assert!(matches!(
        scan_range(&source, &scanner(), regtest(), 1, TIP),
        Err(ScanError::Source(SourceError::RangeUnavailable {
            available_to: 5,
            ..
        }))
    ));
    assert_eq!(source.tip().unwrap().height, 5);

    // A best-block hash whose header the node does not actually have.
    node.set_chain_info("regtest", TIP, node.hashes[TIP as usize]);
    node.set(
        &format!("/rest/headers/{}.bin?count=1", node.hashes[TIP as usize]),
        node.get(&format!("/rest/headers/{}.bin?count=1", node.hashes[1])),
    );
    assert!(matches!(source.tip(), Err(SourceError::InvalidData(_))));
}

#[test]
fn only_loopback_http_urls_are_contacted() {
    for url in [
        "https://127.0.0.1:1",
        "http://192.0.2.1:8332",
        "http://example.com:8332",
    ] {
        match CoreRestSource::connect(url) {
            Err(SourceError::InvalidData(msg)) => {
                assert!(msg.contains("refusing REST URL"), "{msg}")
            }
            other => panic!("{url} must be refused before any request, got {other:?}"),
        }
    }
}
