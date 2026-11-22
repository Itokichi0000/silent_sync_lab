//! The Bitcoin Core REST backend against an in-process fake node serving the
//! fixture chain. No bitcoind needed, so these run in the normal test suite.
//!
//! Two purposes: the backend must give the same result as the fixture adapter
//! on the same chain, and it must refuse a node whose answers do not add up —
//! including one that only *claims* to be on a lab network.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_http::{encode_spent_outputs, fixture_blocks, FakeCoreRest, FakeHttp},
    fixture_gen::{self, LABEL_USED},
};
use silent_sync::{
    bitcoin::{
        blockdata::constants::genesis_block,
        consensus::encode::{deserialize, serialize},
        Block, Network as BitcoinNetwork, TxOut, Witness,
    },
    engine::{scan_range, ScanError, ScanReport},
    network::{ChainIdentity, Network, NetworkError},
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
    ChainIdentity::for_network(Network::Regtest)
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap()
}

fn fresh() -> FakeCoreRest {
    FakeCoreRest::serve(&fixture())
}

/// Scans the whole fake chain; the error is rendered for matching.
fn scan(node: &FakeCoreRest) -> Result<usize, String> {
    let source = CoreRestSource::connect(&node.url()).map_err(|e| e.to_string())?;
    scan_range(&source, &scanner(), regtest(), 1, TIP)
        .map(|r| r.detected_received_outputs.len())
        .map_err(|e| e.to_string())
}

#[track_caller]
fn assert_refused(result: Result<usize, String>, needle: &str) {
    match result {
        Err(e) => assert!(e.contains(needle), "`{e}` should mention `{needle}`"),
        Ok(n) => panic!("expected a refusal mentioning `{needle}`, got {n} detections"),
    }
}

fn outputs(r: &ScanReport) -> Vec<(silent_sync::models::BlockRef, String, u32, u64, Option<u32>)> {
    r.detected_received_outputs
        .iter()
        .map(|d| (d.block, d.txid.to_string(), d.vout, d.value_sats, d.label))
        .collect()
}

#[test]
fn honest_node_gives_the_same_result_as_the_fixture_adapter() {
    let node = fresh();
    let core = CoreRestSource::connect(&node.url()).unwrap();
    let file = FixtureSource::from_json_str(FIXTURE_JSON).unwrap();
    assert_eq!(core.chain(), file.chain());
    assert!(!core.is_pruned());
    assert_eq!(core.tip().unwrap(), file.tip().unwrap());
    for height in 1..=TIP {
        assert_eq!(
            core.block_ref(height).unwrap(),
            file.block_ref(height).unwrap()
        );
    }

    let via_core = scan_range(&core, &scanner(), regtest(), 1, TIP).unwrap();
    let via_file = scan_range(&file, &scanner(), regtest(), 1, TIP).unwrap();
    assert_eq!(via_core.detected_received_outputs.len(), 7);
    // Identical apart from which backend the evidence came from.
    assert_eq!(outputs(&via_core), outputs(&via_file));
    assert_eq!(via_core.txs_skipped, via_file.txs_skipped);
    assert_eq!(
        (via_core.first_block, via_core.last_block),
        (via_file.first_block, via_file.last_block)
    );
    assert_eq!(via_core.backend_id, "bitcoin_core_rest");
    assert_eq!(
        core.fetch_range(3, 5).unwrap(),
        file.fetch_range(3, 5).unwrap()
    );

    let caps = core.capabilities();
    assert!(caps.verifies_merkle_root && caps.verifies_witness_commitment);
    assert!(caps.verifies_proof_of_work && caps.consensus_validated && !caps.remote);
    assert!(core.stats().requests > 20 && core.stats().downloaded_bytes > 3_000);

    // Only the documented endpoints are used, and nothing wallet-specific is sent.
    for request in node.http.requests() {
        assert!(
            request == "/rest/chaininfo.json"
                || request.starts_with("/rest/blockhashbyheight/")
                || request.starts_with("/rest/headers/")
                || request.starts_with("/rest/block/")
                || request.starts_with("/rest/spenttxouts/"),
            "{request}"
        );
    }
}

#[test]
fn mainnet_and_foreign_chains_are_refused_by_name() {
    let node = fresh();
    let best = node.hashes[TIP as usize];

    node.set_chain_info("main", TIP, best);
    assert_mainnet_claim_refused(CoreRestSource::connect(&node.url()).err());
    for chain in ["test", "testnet4", "MAIN"] {
        node.set_chain_info(chain, TIP, best);
        assert!(matches!(
            CoreRestSource::connect(&node.url()),
            Err(SourceError::Network(NetworkError::Unsupported(_)))
        ));
    }

    // Says "regtest" but reports the mainnet genesis block as its block 0.
    node.set_chain_info("regtest", TIP, best);
    let mainnet_genesis = genesis_block(BitcoinNetwork::Bitcoin).block_hash();
    node.set(
        "/rest/blockhashbyheight/0.hex",
        format!("{mainnet_genesis}\n").into_bytes(),
    );
    assert!(matches!(
        CoreRestSource::connect(&node.url()),
        Err(SourceError::Network(NetworkError::GenesisMismatch { .. }))
    ));

    // Says "signet" while serving the regtest chain.
    let node = fresh();
    node.set_chain_info("signet", TIP, best);
    assert!(matches!(
        CoreRestSource::connect(&node.url()),
        Err(SourceError::Network(NetworkError::GenesisMismatch { .. }))
    ));

    // The node changes its mind after we connected.
    let node = fresh();
    let source = CoreRestSource::connect(&node.url()).unwrap();
    node.set_chain_info("main", TIP, best);
    assert!(source.tip().is_err());
    #[cfg(not(feature = "mainnet"))]
    assert_eq!(
        source.tip().err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
}

/// A responder that says "main" while serving the fixture (regtest) chain.
/// Without the `mainnet` feature the name alone is refused; with it, the
/// name is a network, and the chain served is not that network's.
fn assert_mainnet_claim_refused(error: Option<SourceError>) {
    #[cfg(not(feature = "mainnet"))]
    assert_eq!(
        error,
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
    #[cfg(feature = "mainnet")]
    assert!(
        matches!(
            error,
            Some(SourceError::Network(NetworkError::GenesisMismatch { .. }))
        ),
        "{error:?}"
    );
}

/// The name and the reported block 0 are only claims. What binds the data to
/// the lab network is that every block used was reached by following headers
/// from that network's genesis block. A responder that serves another chain's
/// blocks under a lab name — the way real mainnet blocks could be relayed —
/// must be refused even when each block is valid on its own.
#[test]
fn blocks_not_rooted_at_the_network_genesis_are_refused() {
    // The fixture chain re-sealed on top of the *mainnet* genesis hash: every
    // block is internally valid and the chain links from block 1 upward.
    let mut blocks = fixture_blocks(&fixture());
    let mut parent = genesis_block(BitcoinNetwork::Bitcoin).block_hash();
    for (block, _) in &mut blocks {
        block.header.prev_blockhash = parent;
        fixture_gen::seal(block);
        parent = block.block_hash();
    }
    // The fake still answers "regtest" and the regtest genesis hash for height 0.
    let node = FakeCoreRest::serve_blocks(Network::Regtest, &blocks);
    assert!(
        CoreRestSource::connect(&node.url()).is_ok(),
        "the claims alone look fine"
    );
    assert_refused(
        scan(&node),
        "header at height 1 does not link to its parent",
    );
    let source = CoreRestSource::connect(&node.url()).unwrap();
    for result in [
        source.tip().map(|_| ()),
        source.block_ref(1).map(|_| ()),
        source.fetch_range(1, 1).map(|_| ()),
    ] {
        match result {
            Err(SourceError::InvalidData(msg)) => assert!(msg.contains("does not link"), "{msg}"),
            other => panic!("expected the header chain to be refused, got {other:?}"),
        }
    }

    // Starting above height 1 does not get around it.
    assert!(matches!(
        source.fetch_range(5, 6),
        Err(SourceError::InvalidData(_))
    ));

    // A chain that breaks in the middle.
    let node = fresh();
    let mut headers = node.get(&node.headers_path(0));
    headers[80 * 4 + 4] ^= 0x01; // prev_blockhash of the header at height 4
    node.set(&node.headers_path(0), headers);
    assert_refused(
        scan(&node),
        "header at height 4 does not link to its parent",
    );

    // A header without valid proof of work.
    let node = fresh();
    let mut headers = node.get(&node.headers_path(0));
    let mut header: silent_sync::bitcoin::block::Header =
        deserialize(&headers[80 * 8..80 * 9]).unwrap();
    while header.validate_pow(header.target()).is_ok() {
        header.nonce += 1;
    }
    headers[80 * 8..80 * 9].copy_from_slice(&serialize(&header));
    node.set(&node.headers_path(0), headers);
    assert_refused(scan(&node), "header at height 8: proof of work");

    // Malformed header responses.
    let node = fresh();
    let mut headers = node.get(&node.headers_path(0));
    headers.push(0);
    node.set(&node.headers_path(0), headers);
    assert_refused(scan(&node), "not a whole number of headers");
    let node = fresh();
    node.set(&node.headers_path(0), node.get(&node.headers_path(1)));
    assert_refused(scan(&node), "does not start at the requested header");
    let node = fresh();
    node.set(&node.headers_path(0), Vec::new());
    assert_refused(scan(&node), "not on the node's active chain");
}

/// A block served without witnesses (as a node returns for a non-witness
/// request) hashes the same and must still be refused.
#[test]
fn witness_stripped_blocks_from_the_node_are_refused() {
    let node = fresh();
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
    let node = fresh();
    let mut block: Block = deserialize(&node.get(&path)).unwrap();
    block.txdata[1].input[0].witness = Witness::new();
    node.set(&path, serialize(&block));
    assert_refused(scan(&node), "block 2: witness commitment");
}

#[test]
fn inconsistent_block_responses_are_refused() {
    assert_eq!(scan(&fresh()), Ok(7));

    // The body of another block: wrong parent.
    let node = fresh();
    node.set(&node.block_path(3), node.get(&node.block_path(4)));
    assert_refused(scan(&node), "block 3: prev_blockhash");

    // A *different valid block with the right parent* in place of the one
    // asked for: the tip block re-sealed without its paying transaction. Only
    // the comparison with the requested hash can catch this, and without it
    // the payment in that block would silently disappear.
    let node = fresh();
    let mut block: Block = deserialize(&node.get(&node.block_path(TIP))).unwrap();
    block.txdata.pop();
    fixture_gen::seal(&mut block);
    assert_ne!(block.block_hash(), node.hashes[TIP as usize]);
    node.set(&node.block_path(TIP), serialize(&block));
    node.set(&node.spent_path(TIP), encode_spent_outputs(&[vec![]]));
    assert_refused(scan(&node), "block 8: block hash is");

    // Transactions altered without re-sealing.
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

    // Missing data.
    let node = fresh();
    node.remove(&node.spent_path(4));
    assert_refused(scan(&node), "HTTP 404");
    let node = fresh();
    node.remove(&node.block_path(8));
    assert_refused(scan(&node), "HTTP 404");
}

#[test]
fn inconsistent_spent_output_responses_are_refused() {
    // Spent outputs of another block.
    let node = fresh();
    node.set(&node.spent_path(3), node.get(&node.spent_path(2)));
    assert_refused(scan(&node), "block 3: 3 transactions but 2 prevout lists");
    // Truncated / trailing byte.
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
    // One prevout missing; prevouts claimed for the coinbase.
    let node = fresh();
    node.set(
        &node.spent_path(3),
        encode_spent_outputs(&[vec![], vec![], vec![]]),
    );
    assert_refused(scan(&node), "block 3: transaction");
    let node = fresh();
    let some_output = TxOut {
        value: silent_sync::bitcoin::Amount::from_sat(1),
        script_pubkey: silent_sync::bitcoin::ScriptBuf::new(),
    };
    node.set(
        &node.spent_path(1),
        encode_spent_outputs(&[vec![some_output]]),
    );
    assert_refused(
        scan(&node),
        "has 0 inputs needing prevouts but 1 were supplied",
    );
}

/// What the backend cannot notice (recorded so the limit stays visible):
/// nothing commits to the spent outputs, so a responder that swaps them for
/// well-formed lies hides every payment while the scan succeeds. Only outputs
/// created inside the fetched range are cross-checked.
#[test]
fn lying_about_spent_outputs_from_outside_the_range_goes_unnoticed() {
    let node = fresh();
    let mut blocks = fixture_blocks(&fixture());
    for (height, (_, prevouts)) in blocks.iter_mut().enumerate() {
        for tx in prevouts.iter_mut() {
            for prevout in tx.iter_mut() {
                // A P2WSH script: makes every input "not eligible".
                prevout.script_pubkey = silent_sync::bitcoin::ScriptBuf::from_hex(
                    "0020000000000000000000000000000000000000000000000000000000000000dead",
                )
                .unwrap();
            }
        }
        node.set(
            &node.spent_path(height as u32 + 1),
            encode_spent_outputs(prevouts),
        );
    }
    assert_eq!(scan(&node), Ok(0), "all 7 payments silently missed");
}

#[test]
fn the_tip_is_bound_to_the_verified_header_chain() {
    let node = fresh();
    let source = CoreRestSource::connect(&node.url()).unwrap();
    assert_eq!(source.tip().unwrap().height, TIP);

    // Height and best-block hash that do not belong together.
    node.set_chain_info("regtest", 3, node.hashes[8]);
    assert!(matches!(source.tip(), Err(SourceError::Unavailable(m)) if m.contains("tip moved")));
    // A height the chain does not reach.
    node.set_chain_info("regtest", 4_000_000, node.hashes[2]);
    assert!(matches!(
        source.tip(),
        Err(SourceError::RangeUnavailable {
            available_to: 8,
            ..
        })
    ));
    assert!(matches!(
        source.fetch_range(1, 9),
        Err(SourceError::RangeUnavailable { .. })
    ));

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
}

#[test]
fn ranges_outside_the_node_s_chain_are_refused() {
    let node = fresh();
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
    assert!(matches!(
        source.block_ref(TIP + 1),
        Err(SourceError::RangeUnavailable { .. })
    ));
    assert_eq!(
        source.block_ref(0).unwrap().hash,
        Network::Regtest.genesis_hash()
    );
}

/// The node reorganizes between two calls: the verified header chain must
/// follow the node's current chain, not keep serving the old one.
#[test]
fn the_header_anchor_follows_a_reorg_on_the_node() {
    let main = fixture();
    let fork = fixture_gen::generate_fork();
    let mut node = FakeCoreRest::serve(&main);
    let source = CoreRestSource::connect(&node.url()).unwrap();
    let on_main = FixtureSource::from_file(main.clone()).unwrap();
    let on_fork = FixtureSource::from_file(fork.clone()).unwrap();
    assert_eq!(source.tip().unwrap(), on_main.tip().unwrap());
    assert_eq!(
        source.fetch_range(6, 8).unwrap(),
        on_main.fetch_range(6, 8).unwrap()
    );

    node.replace_chain(Network::Regtest, &fixture_blocks(&fork));
    assert_eq!(source.tip().unwrap(), on_fork.tip().unwrap());
    for height in 1..=9 {
        assert_eq!(
            source.block_ref(height).unwrap(),
            on_fork.block_ref(height).unwrap()
        );
    }
    assert_eq!(
        source.fetch_range(1, 9).unwrap(),
        on_fork.fetch_range(1, 9).unwrap()
    );

    // And back to the shorter branch.
    node.replace_chain(Network::Regtest, &fixture_blocks(&main));
    assert_eq!(source.tip().unwrap(), on_main.tip().unwrap());
    assert_eq!(source.block_ref(7).unwrap(), on_main.block_ref(7).unwrap());
    assert!(matches!(
        source.block_ref(9),
        Err(SourceError::RangeUnavailable { .. })
    ));
}

/// Redirects are never followed (a redirect could point off the machine) and
/// only a plain 200 counts as an answer.
#[test]
fn redirects_and_non_200_statuses_are_errors() {
    let elsewhere = FakeHttp::new();
    elsewhere.set("/rest/chaininfo.json", fresh().get("/rest/chaininfo.json"));
    let target = format!("{}/rest/chaininfo.json", elsewhere.url());

    for status in [301, 302, 303, 307, 308] {
        let node = fresh();
        node.http.set_reply(
            "/rest/chaininfo.json",
            status,
            &[("Location", &target)],
            Vec::new(),
        );
        assert!(
            matches!(
                CoreRestSource::connect(&node.url()),
                Err(SourceError::Unavailable(_))
            ),
            "{status}"
        );
    }
    assert!(
        elsewhere.requests().is_empty(),
        "a redirect was followed: {:?}",
        elsewhere.requests()
    );

    for status in [201, 204, 206, 400, 500, 503] {
        let node = fresh();
        let body = node.get("/rest/chaininfo.json");
        node.http
            .set_reply("/rest/chaininfo.json", status, &[], body);
        match CoreRestSource::connect(&node.url()) {
            Err(SourceError::Unavailable(msg)) => {
                assert!(msg.contains(&format!("HTTP {status}")), "{msg}")
            }
            other => panic!("status {status} must be an error, got {other:?}"),
        }
    }

    // Error bodies are echoed without control characters and bounded.
    let node = fresh();
    node.http.set_reply(
        "/rest/chaininfo.json",
        500,
        &[],
        format!("bad\r\n\u{1b}[31mthing{}", "x".repeat(5_000)).into_bytes(),
    );
    let message = CoreRestSource::connect(&node.url())
        .unwrap_err()
        .to_string();
    assert!(!message.contains('\r') && !message.contains('\n') && !message.contains('\u{1b}'));
    assert!(message.len() < 400, "{}", message.len());
}

#[test]
fn only_loopback_http_urls_are_contacted() {
    for url in [
        "https://127.0.0.1:1",
        "http://192.0.2.1:8332",
        "http://example.com:8332",
        "http://localhost:18443",
        "http://[::1]:18443",
        "http://127.0.0.1",
    ] {
        match CoreRestSource::connect(url) {
            Err(SourceError::InvalidData(msg)) => {
                assert!(msg.contains("refusing REST URL"), "{msg}")
            }
            other => panic!("{url} must be refused before any request, got {other:?}"),
        }
    }
}

/// A node that fails to answer is not a node whose chain got shorter: one
/// failed request ends the call, and the verified header chain is kept.
#[test]
fn an_unreachable_node_costs_one_request_and_keeps_the_verified_headers() {
    let node = fresh();
    let source = CoreRestSource::connect(&node.url()).unwrap();
    assert_eq!(source.tip().unwrap().height, TIP);
    let top = format!("/rest/blockhashbyheight/{TIP}.hex");
    let honest = node.get(&top);

    // The node answers the freshness check with a server error.
    node.http.set_reply(&top, 503, &[], b"warming up".to_vec());
    let before = source.stats().requests;
    assert!(matches!(
        source.block_ref(5),
        Err(SourceError::Unavailable(m)) if m.contains("HTTP 503")
    ));
    assert_eq!(
        source.stats().requests - before,
        1,
        "the cache must not be probed entry by entry"
    );

    // Back to normal: the headers are still there, nothing is downloaded again.
    node.set(&top, honest);
    let before = source.stats().requests;
    assert_eq!(source.block_ref(5).unwrap().hash, node.hashes[5]);
    assert_eq!(
        source.stats().requests - before,
        1,
        "only the freshness check"
    );

    // The node is gone altogether.
    let url = node.url();
    drop(node);
    let before = source.stats().requests;
    assert!(matches!(
        source.block_ref(5),
        Err(SourceError::Unavailable(_))
    ));
    assert!(matches!(source.tip(), Err(SourceError::Unavailable(_))));
    assert_eq!(source.stats().requests - before, 2);
    assert!(CoreRestSource::connect(&url).is_err());
}

/// After a deep reorg the fork point is found with a handful of requests, and
/// a whole range of block references costs one freshness check.
#[test]
fn deep_reorgs_and_range_lookups_cost_few_requests() {
    use lab_testkit::fixture_gen::LongChain;
    let chain = |payment_every| {
        fixture_gen::generate_long(&LongChain {
            payment_every,
            ..LongChain::standard(300)
        })
    };
    let (main, other) = (chain(10), chain(7));
    let mut node = FakeCoreRest::serve(&main);
    let source = CoreRestSource::connect(&node.url()).unwrap();
    assert_eq!(source.tip().unwrap().height, 300);

    // A range of references: one request, whatever its length.
    let before = source.stats().requests;
    let refs = source.block_refs(1, 300).unwrap();
    assert_eq!(source.stats().requests - before, 1);
    assert_eq!(refs.len(), 300);
    for (index, block) in refs.iter().enumerate() {
        assert_eq!(block.height as usize, index + 1);
        assert_eq!(block.hash, node.hashes[index + 1]);
        assert_eq!(block.prev_hash, node.hashes[index]);
    }
    assert_eq!(source.block_refs(250, 250).unwrap(), vec![refs[249]]);
    assert!(matches!(
        source.block_refs(5, 4),
        Err(SourceError::InvalidRange { from: 5, to: 4 })
    ));
    assert!(matches!(
        source.block_refs(290, 301),
        Err(SourceError::RangeUnavailable { .. })
    ));

    // The node reorganizes from height 7 on (the chains share blocks 1..=6).
    node.replace_chain(other.network, &fixture_blocks(&other));
    assert_eq!(node.hashes[6], refs[5].hash);
    assert_ne!(node.hashes[7], refs[6].hash);
    let before = source.stats().requests;
    let tip = source.tip().unwrap();
    let spent = source.stats().requests - before;
    assert_eq!(tip.hash, node.hashes[300]);
    assert!(
        spent <= 15,
        "re-anchoring 294 blocks took {spent} requests (bisection expected)"
    );
    let after = source.block_refs(1, 300).unwrap();
    assert_eq!(after[5], refs[5], "below the fork nothing changed");
    for (index, block) in after.iter().enumerate() {
        assert_eq!(block.hash, node.hashes[index + 1]);
    }

    // The node's chain becomes shorter than the cache (a definite answer).
    let short = chain(10);
    let blocks = fixture_blocks(&short);
    node.replace_chain(short.network, &blocks[..40]);
    assert_eq!(source.tip().unwrap().height, 40);
    assert!(matches!(
        source.block_ref(41),
        Err(SourceError::RangeUnavailable {
            available_to: 40,
            ..
        })
    ));
}
