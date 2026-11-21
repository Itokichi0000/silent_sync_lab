//! The BlindBit Oracle v2 backend against in-process fakes of the oracle and
//! of the node that supplies headers. The fake oracle answers in the shape
//! observed from the real one (docs/RESEARCH.md §4.1), including its habit of
//! answering unknown heights with HTTP 200 and an empty hash.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_http::{oracle_full_block_json, FakeCoreRest, FakeOracle},
    fixture_gen::{self, LABEL_USED},
};
use silent_sync::{
    engine::scan_range,
    models::EvidenceKind,
    network::{ChainIdentity, Network, NetworkError},
    protocol::ReceiveScanner,
    source::{
        bitcoin_core::CoreRestSource,
        blindbit::BlindbitHttpSource,
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
        BlockEvidence, ScanSource, SourceError,
    },
    verify::{cross_check, Explanation},
};

const TIP: u32 = 8;

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(Network::Regtest)
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap()
}

/// What an oracle with the observed behaviour would index for the fixture.
fn index_blocks(
    fixture: &silent_sync::source::fixture::FixtureFile,
    to: u32,
) -> Vec<BlockEvidence> {
    LocalTweakIndex::new(
        FixtureSource::from_file(fixture.clone()).unwrap(),
        IndexPolicy::IgnoreSegwitVersionRule,
    )
    .fetch_range(1, to)
    .unwrap()
    .blocks
}

struct Setup {
    node: FakeCoreRest,
    oracle: FakeOracle,
}

impl Setup {
    fn new() -> Self {
        let fixture = fixture_gen::generate();
        Self {
            node: FakeCoreRest::serve(&fixture),
            oracle: FakeOracle::serve("regtest", &index_blocks(&fixture, TIP)),
        }
    }

    fn connect(&self) -> Result<BlindbitHttpSource<CoreRestSource>, SourceError> {
        BlindbitHttpSource::connect(
            &self.oracle.url(),
            CoreRestSource::connect(&self.node.url())?,
        )
    }

    fn scan(&self) -> Result<usize, String> {
        let source = self.connect().map_err(|e| e.to_string())?;
        scan_range(&source, &scanner(), regtest(), 1, TIP)
            .map(|r| r.detected_received_outputs.len())
            .map_err(|e| e.to_string())
    }
}

#[track_caller]
fn assert_refused(result: Result<usize, String>, needle: &str) {
    match result {
        Err(e) => assert!(e.contains(needle), "`{e}` should mention `{needle}`"),
        Ok(n) => panic!("expected a refusal mentioning `{needle}`, got {n} detections"),
    }
}

#[test]
fn oracle_backend_scans_the_index_and_differs_from_full_blocks_as_observed() {
    let setup = Setup::new();
    let oracle = setup.connect().unwrap();
    assert_eq!(oracle.backend_id(), "blindbit_oracle_v2_http");
    assert_eq!(oracle.chain(), regtest());
    let caps = oracle.capabilities();
    assert_eq!(caps.evidence_kind, EvidenceKind::TweakIndex);
    assert!(!caps.verifies_merkle_root && !caps.consensus_validated);
    // Block identity comes from the header source.
    let node = CoreRestSource::connect(&setup.node.url()).unwrap();
    assert_eq!(oracle.tip().unwrap(), node.tip().unwrap());
    assert_eq!(oracle.block_ref(5).unwrap(), node.block_ref(5).unwrap());

    let report = scan_range(&oracle, &scanner(), regtest(), 1, TIP).unwrap();
    assert_eq!(report.detected_received_outputs.len(), 8);
    assert_eq!(report.txs_seen, 7);
    assert_eq!(report.last_block, node.tip().unwrap());

    // Against the node itself: 7 agree, 1 extra with a precise reason.
    let check = cross_check(&oracle, &node, &scanner(), regtest(), 1, TIP).unwrap();
    assert_eq!(
        (
            check.agreed,
            check.candidate_only,
            check.reference_only,
            check.unexplained
        ),
        (7, 1, 0, 0)
    );
    assert_eq!(
        check.differences[0].explanation,
        Explanation::ReferenceNotEligible {
            reason: "spends_unknown_segwit_version"
        }
    );
    assert_eq!(check.candidate_backend, "blindbit_oracle_v2_http");
    assert_eq!(check.reference_backend, "bitcoin_core_rest");

    // Requests to the oracle carry a height and nothing else.
    for request in setup.oracle.http.requests() {
        assert!(
            request == "/info" || request.starts_with("/full-block/"),
            "{request}"
        );
        assert!(request.len() < 20, "{request}");
    }
    assert!(oracle.stats().requests >= 9 && oracle.stats().downloaded_bytes > 1_000);
}

#[test]
fn an_oracle_that_is_behind_is_not_mistaken_for_empty_blocks() {
    // The oracle has indexed only up to height 5; the node is at 8.
    let fixture = fixture_gen::generate();
    let setup = Setup {
        node: FakeCoreRest::serve(&fixture),
        oracle: FakeOracle::serve("regtest", &index_blocks(&fixture, 5)),
    };
    let source = setup.connect().unwrap();
    assert_eq!(
        source.tip().unwrap().height,
        TIP,
        "the tip is the header source's"
    );
    assert_eq!(
        source.fetch_range(1, TIP),
        Err(SourceError::RangeUnavailable {
            from: 1,
            to: TIP,
            available_from: 1,
            available_to: 5
        })
    );
    assert_eq!(source.fetch_range(1, 5).unwrap().blocks.len(), 5);

    // The oracle *claims* to be at 8, but answers heights 6..8 the way the
    // real one answers unknown heights: HTTP 200, empty hash, null index.
    setup.oracle.set_info("regtest", TIP, false);
    assert_refused(
        setup.scan(),
        "oracle block 6: the oracle has no data for this height",
    );
}

#[test]
fn an_oracle_on_another_chain_or_network_is_refused() {
    // Indexed the fork; the header source is on the main chain.
    let main = fixture_gen::generate();
    let fork = fixture_gen::generate_fork();
    let setup = Setup {
        node: FakeCoreRest::serve(&main),
        oracle: FakeOracle::serve("regtest", &index_blocks(&fork, 9)),
    };
    assert_eq!(
        scan_range(&setup.connect().unwrap(), &scanner(), regtest(), 1, 5)
            .unwrap()
            .blocks_scanned,
        5
    );
    assert_refused(setup.scan(), "oracle block 6: the oracle indexed block");

    let setup = Setup::new();
    setup.oracle.set_info("main", TIP, false);
    assert_eq!(
        setup.connect().err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
    setup.oracle.set_info("mainnet", TIP, false);
    assert_eq!(
        setup.connect().err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
    setup.oracle.set_info("testnet", TIP, false);
    assert!(matches!(
        setup.connect(),
        Err(SourceError::Network(NetworkError::Unsupported(_)))
    ));
    setup.oracle.set_info("signet", TIP, false);
    assert!(
        matches!(setup.connect(), Err(SourceError::InvalidData(m)) if m.contains("indexes signet"))
    );
    setup.oracle.set_info("regtest", TIP, true);
    assert!(
        matches!(setup.connect(), Err(SourceError::InvalidData(m)) if m.contains("tweaks_only"))
    );

    // The header source's own refusals apply first.
    let setup = Setup::new();
    setup
        .node
        .set_chain_info("main", TIP, setup.node.hashes[TIP as usize]);
    assert_eq!(
        setup.connect().err(),
        Some(SourceError::Network(NetworkError::MainnetRefused))
    );
}

#[test]
fn malformed_oracle_answers_are_refused() {
    let with = |height: u32, body: Vec<u8>| {
        let setup = Setup::new();
        setup.oracle.http.set(&FakeOracle::block_path(height), body);
        setup.scan()
    };
    let honest = Setup::new();
    assert_eq!(honest.scan(), Ok(8));
    let block3 = honest.oracle.http.get(&FakeOracle::block_path(3));
    let block4 = honest.oracle.http.get(&FakeOracle::block_path(4));

    // The answer for another height.
    assert_refused(with(3, block4), "oracle block 3: answer is for height 4");
    // Not JSON / wrong shape.
    assert_refused(with(3, b"<html>".to_vec()), "oracle block 3:");
    assert_refused(with(3, b"{}".to_vec()), "oracle block 3:");
    // Hash that is not a hash.
    let text = String::from_utf8(block3.clone()).unwrap();
    let hash = honest.node.hashes[3].to_string();
    assert_refused(
        with(3, text.replace(&hash, "zz").into_bytes()),
        "block hash is not a hash",
    );
    // Tweak / output key / txid that do not parse.
    let value: serde_json::Value = serde_json::from_slice(&block3).unwrap();
    let edit = |f: &dyn Fn(&mut serde_json::Value)| {
        let mut v = value.clone();
        f(&mut v);
        with(3, v.to_string().into_bytes())
    };
    assert_refused(
        edit(&|v| v["index"][0]["tweak"] = "02".into()),
        "oracle block 3:",
    );
    assert_refused(
        edit(&|v| v["index"][0]["txid"] = "abc".into()),
        "oracle block 3:",
    );
    assert_refused(
        edit(&|v| v["index"][0]["utxos"][0]["pubkey"] = "00ff".into()),
        "bad output key",
    );
    assert_refused(
        edit(&|v| v["index"][0]["utxos"][0]["amount"] = "-1".into()),
        "oracle block 3:",
    );
    // An output listed twice.
    assert_refused(
        edit(&|v| {
            let first = v["index"][0]["utxos"][0].clone();
            v["index"][0]["utxos"].as_array_mut().unwrap().push(first);
        }),
        "twice",
    );
    // HTTP errors.
    let setup = Setup::new();
    setup.oracle.http.remove(&FakeOracle::block_path(7));
    assert_refused(setup.scan(), "HTTP 404");
    let setup = Setup::new();
    setup.oracle.http.set_reply(
        "/info",
        302,
        &[("Location", "http://127.0.0.1:1/info")],
        vec![],
    );
    assert!(matches!(setup.connect(), Err(SourceError::Unavailable(_))));

    // Fields an answer of the known shape always has.
    assert_refused(
        edit(&|v| {
            v["index"][0].as_object_mut().unwrap().remove("utxos");
        }),
        "oracle block 3:",
    );
    assert_refused(
        edit(&|v| v["index"][0]["utxos"] = serde_json::json!([])),
        "lists no outputs",
    );
    // More money than exists.
    assert_refused(
        edit(&|v| v["index"][0]["utxos"][0]["amount"] = serde_json::json!(u64::MAX)),
        "more than can exist",
    );
    let setup = Setup::new();
    setup
        .oracle
        .http
        .set("/info", br#"{"network":"regtest","height":8}"#.to_vec());
    assert!(
        matches!(setup.connect(), Err(SourceError::InvalidData(m)) if m.contains("tweaks_only"))
    );
    // Error text taken from an answer is bounded, however large the answer.
    let setup = Setup::new();
    setup.oracle.http.set(
        "/info",
        format!(
            r#"{{"network":"regtest","height":"{}","tweaks_only":false}}"#,
            "A".repeat(100_000)
        )
        .into_bytes(),
    );
    let message = setup.connect().err().unwrap().to_string();
    assert!(message.len() < 400, "{} bytes of error text", message.len());
    let setup = Setup::new();
    setup.node.set(
        "/rest/chaininfo.json",
        format!(
            r#"{{"chain":"regtest","blocks":"{}"}}"#,
            "B".repeat(100_000)
        )
        .into_bytes(),
    );
    let message = setup.connect().err().unwrap().to_string();
    assert!(message.len() < 400, "{} bytes of error text", message.len());

    // `index: null` with a correct hash is an empty block, not an error.
    let node_hash = honest.node.hashes[1];
    let mut empty: serde_json::Value =
        serde_json::from_slice(&oracle_full_block_json(1, Some(node_hash), &[])).unwrap();
    empty["index"] = serde_json::Value::Null;
    assert_eq!(with(1, empty.to_string().into_bytes()), Ok(8));
}

#[test]
fn only_loopback_oracles_are_contacted() {
    let fixture = fixture_gen::generate();
    let node = FakeCoreRest::serve(&fixture);
    for url in [
        "https://127.0.0.1:27000",
        "http://oracle.example.com:8000",
        "http://localhost:8000",
        "http://10.0.0.5:8000",
    ] {
        let headers = CoreRestSource::connect(&node.url()).unwrap();
        match BlindbitHttpSource::connect(url, headers) {
            Err(SourceError::InvalidData(msg)) => {
                assert!(msg.contains("refusing oracle URL"), "{msg}")
            }
            other => panic!("{url} must be refused, got {:?}", other.map(|_| ())),
        }
    }
}

/// The header source is asked once per fetched range, not once per block.
#[test]
fn block_identity_for_a_range_costs_one_header_lookup() {
    use lab_testkit::fixture_gen::LongChain;
    let fixture = fixture_gen::generate_long(&LongChain::standard(150));
    let node = FakeCoreRest::serve(&fixture);
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture).unwrap(),
        IndexPolicy::Bip352,
    )
    .fetch_range(1, 150)
    .unwrap()
    .blocks;
    let oracle_server = FakeOracle::serve("regtest", &index);
    let oracle = BlindbitHttpSource::connect(
        &oracle_server.url(),
        CoreRestSource::connect(&node.url()).unwrap(),
    )
    .unwrap();
    assert_eq!(oracle.tip().unwrap().height, 150);

    let before = oracle.header_source().stats().requests;
    let report = scan_range(&oracle, &scanner(), regtest(), 1, 150).unwrap();
    let header_requests = oracle.header_source().stats().requests - before;
    assert_eq!(report.detected_received_outputs.len(), 15);
    // Two chunks (100 + 50 blocks): one freshness check each.
    assert_eq!(
        header_requests, 2,
        "header lookups must not grow with the range"
    );
    // The backend's own counters cover both connections.
    assert!(oracle.stats().requests >= 150 + header_requests);
}
