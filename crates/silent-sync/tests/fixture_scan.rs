//! Gate 1 — fixture-backed detection through the whole `silent-sync` path
//! (fixture adapter → engine → scanner wrapper → upstream scanner).
//!
//! Covers design-spec cases T02–T05 at fixture scale, wrong-key behaviour,
//! the BIP352 eligibility rules `silent-sync` adds on top of upstream, and the
//! fail-closed behaviour of the adapter and the engine.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use bdk_sp::receive::scan::Scanner;
use lab_testkit::fixture_gen::{
    self, LABEL_NOT_BACKED_UP, LABEL_USED, LAB_RECEIVER, OTHER_RECEIVER,
};
use silent_sync::{
    bitcoin::{
        absolute::LockTime,
        consensus::encode::{deserialize_hex, serialize_hex},
        key::TweakedPublicKey,
        script::Builder,
        transaction::Version,
        Amount, Block, CompactTarget, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
        Txid, Witness,
    },
    engine::{scan_range, ScanError, ScanReport},
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, Network},
    protocol::{ProtocolError, ReceiveScanner, ReceiverKeys, SkipReason, TxScanOutcome},
    source::{
        fixture::{FixtureExpectedDetection, FixtureFile, FixtureSource},
        Evidence, ScanBatch, ScanSource, SourceCapabilities, SourceError,
    },
};
use std::collections::BTreeMap;

const FIXTURE_JSON: &str = include_str!("../../../fixtures/local/regtest_basic_v1.json");
const TIP: u32 = 8;

fn source() -> FixtureSource {
    FixtureSource::from_json_str(FIXTURE_JSON).unwrap()
}

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(Network::Regtest)
}

fn scan_all(scanner: &ReceiveScanner) -> ScanReport {
    scan_range(&source(), scanner, regtest(), 1, TIP).unwrap()
}

/// (height, txid, vout, value, label) of what was detected.
fn detected(report: &ScanReport) -> Vec<(u32, Txid, u32, u64, Option<u32>)> {
    report
        .detected_received_outputs
        .iter()
        .map(|d| (d.block.height, d.txid, d.vout, d.value_sats, d.label))
        .collect()
}

/// What a wallet knowing `labels` must find, derived from sender-side
/// knowledge only: per transaction, outputs are found in `k` order and the
/// search stops at the first output whose label the wallet does not know.
fn expected(labels: &[u32]) -> Vec<(u32, Txid, u32, u64, Option<u32>)> {
    let src = source();
    let mut by_tx: BTreeMap<(u32, Txid), Vec<&FixtureExpectedDetection>> = BTreeMap::new();
    for e in src.expected_detections() {
        by_tx.entry((e.height, e.txid)).or_default().push(e);
    }
    let mut found = Vec::new();
    for (_, mut outputs) in by_tx {
        outputs.sort_by_key(|e| e.k);
        for (position, e) in outputs.iter().enumerate() {
            assert_eq!(e.k as usize, position, "fixture: k must be 0, 1, 2, ...");
        }
        found.extend(
            outputs
                .into_iter()
                .take_while(|e| e.label.is_none_or(|m| labels.contains(&m)))
                .map(|e| (e.height, e.txid, e.vout, e.value_sats, e.label)),
        );
    }
    // Same order as the engine: height, position in block, vout. Each fixture
    // block holds at most one transaction that pays the receiver.
    found.sort_by_key(|e| (e.0, e.2));
    found
}

#[test]
fn committed_fixture_is_exactly_what_the_generator_produces() {
    assert_eq!(
        FIXTURE_JSON,
        fixture_gen::generate_json(),
        "fixtures/local/regtest_basic_v1.json is stale; regenerate with \
         `cargo run -p lab-testkit --bin gen_fixture -- fixtures/local/regtest_basic_v1.json`"
    );
}

#[test]
fn detects_exactly_the_outputs_created_for_the_receiver() {
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap();
    let report = scan_all(&scanner);

    let want = expected(&[0, LABEL_USED]);
    assert_eq!(want.len(), 7, "fixture sanity");
    assert_eq!(detected(&report), want);

    // T02: single payment.
    assert_eq!((want[0].0, want[0].2, want[0].3), (2, 0, 50_000));
    // T03/T04: two outputs of one transaction, k = 0 and k = 1, non-zero vouts.
    assert_eq!(want[1].1, want[2].1);
    assert_eq!((want[1].2, want[2].2), (1, 2));
    // T05: labelled output and change label in one transaction ...
    assert_eq!(want[3].1, want[4].1);
    assert_eq!((want[3].4, want[4].4), (Some(LABEL_USED), Some(0)));
    // ... and a change-label-only payment.
    assert_eq!((want[5].0, want[5].2, want[5].4), (7, 1, Some(0)));
    // A 1 sat output is detected like any other.
    assert_eq!(want[6].3, 1);

    assert_eq!(report.backend_id, "fixture");
    assert_eq!(report.chain, regtest());
    assert_eq!(
        (report.first_block.height, report.last_block.height),
        (1, TIP)
    );
    assert_eq!(report.labels_scanned, vec![0, LABEL_USED]);
    assert_eq!(report.blocks_scanned, 8);
    assert_eq!(report.txs_seen, 17);
    // Heights 2, 3 (two, one of them someone else's payment), 5, 7, 8.
    assert_eq!(report.txs_scanned, 6);
    let skipped: BTreeMap<&str, u64> = [
        ("coinbase", 8),
        ("no_eligible_inputs", 1),
        ("no_taproot_outputs", 1),
        ("spends_unknown_segwit_version", 1),
    ]
    .into_iter()
    .collect();
    assert_eq!(report.txs_skipped, skipped);
    for d in &report.detected_received_outputs {
        assert_eq!(d.network, Network::Regtest);
        assert_eq!(d.provenance.backend_id, "fixture");
        assert_eq!(
            d.provenance.evidence_kind,
            EvidenceKind::FullBlockWithPrevouts
        );
    }
}

#[test]
fn sub_ranges_detect_the_matching_subset() {
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap();
    let all = expected(&[0, LABEL_USED]);
    for (from, to) in [(1, 1), (2, 2), (3, 5), (6, 8), (4, 4), (1, 8)] {
        let report = scan_range(&source(), &scanner, regtest(), from, to).unwrap();
        let want: Vec<_> = all
            .iter()
            .filter(|e| (from..=to).contains(&e.0))
            .cloned()
            .collect();
        assert_eq!(detected(&report), want, "{from}..={to}");
        assert_eq!(report.blocks_scanned, u64::from(to - from + 1));
    }
}

/// Restoring from keys alone is not a full restore (design spec §6).
///
/// The fixture's height-5 transaction pays label 1 at k = 0, the change label
/// at k = 1 and label 5 at k = 2. BIP352 scanning walks k upwards and stops at
/// the first k without a match, so a wallet that lacks an early label loses
/// every later output of that transaction — including ones whose label it
/// does know.
#[test]
fn labels_decide_what_a_restored_wallet_can_see() {
    let keys = fixture_gen::lab_receiver_keys();
    let count = |labels: &[u32]| {
        let report = scan_all(&ReceiveScanner::new(&keys, labels).unwrap());
        let mut known = labels.to_vec();
        known.push(0); // `ReceiveScanner::new` always scans the change label
        assert_eq!(detected(&report), expected(&known), "labels {labels:?}");
        let at_5 = report
            .detected_received_outputs
            .iter()
            .filter(|d| d.block.height == 5)
            .count();
        (report.detected_received_outputs.len(), at_5)
    };

    // Everything backed up: all 8 outputs.
    assert_eq!(count(&[LABEL_USED, LABEL_NOT_BACKED_UP]), (8, 3));
    // Label 5 lost: only that output is missed.
    assert_eq!(count(&[LABEL_USED]), (7, 2));
    // Keys only: label 1 at k = 0 is unknown, so the change output behind it
    // (k = 1) is missed too. The change-only payment at height 7 is found.
    assert_eq!(count(&[]), (5, 0));
    // Label 5 known but label 1 lost: label 5 sits at k = 2 behind the unknown
    // k = 0 output and is missed although the wallet knows it.
    assert_eq!(count(&[LABEL_NOT_BACKED_UP]), (5, 0));

    // Conformance-only constructor: without the change label no change
    // output is found at all, which is why `ReceiveScanner::new` always adds it.
    let no_change = scan_all(&ReceiveScanner::with_exact_labels(&keys, &[]).unwrap());
    assert_eq!(detected(&no_change).len(), 4);
    assert!(detected(&no_change).iter().all(|d| d.4.is_none()));
}

#[test]
fn other_keys_do_not_detect_the_receivers_outputs() {
    let (lab_scan, lab_spend) = fixture_gen::vector_raw_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    let (other_scan, other_spend) =
        fixture_gen::vector_raw_keys(OTHER_RECEIVER.0, OTHER_RECEIVER.1);
    let labels = [LABEL_USED, LABEL_NOT_BACKED_UP];

    // A different receiver sees only the one payment made to it.
    let other = scan_all(
        &ReceiveScanner::new(&ReceiverKeys::new(other_scan, other_spend), &labels).unwrap(),
    );
    assert_eq!(other.detected_received_outputs.len(), 1);
    let theirs = &other.detected_received_outputs[0];
    assert_eq!(
        (theirs.block.height, theirs.vout, theirs.value_sats),
        (3, 0, 90_000)
    );
    let ours = expected(&[0, LABEL_USED, LABEL_NOT_BACKED_UP]);
    assert!(ours
        .iter()
        .all(|e| (e.1, e.2) != (theirs.txid, theirs.vout)));

    // Mixing one right key with one wrong key detects nothing at all.
    for keys in [
        ReceiverKeys::new(lab_scan, other_spend),
        ReceiverKeys::new(other_scan, lab_spend),
    ] {
        let report = scan_all(&ReceiveScanner::new(&keys, &labels).unwrap());
        assert!(report.detected_received_outputs.is_empty());
        assert_eq!(report.txs_scanned, 6);
    }
}

fn fixture_txs(height: u32) -> Vec<(Transaction, Vec<TxOut>)> {
    let batch = source().fetch_range(height, height).unwrap();
    let Evidence::FullBlock(txs) = &batch.blocks[0].evidence else {
        panic!("fixture always yields full blocks");
    };
    txs.iter()
        .map(|t| (t.tx.clone(), t.prevouts.clone()))
        .collect()
}

/// D-006: upstream alone would report an output in a transaction that spends
/// a SegWit v2 output; BIP352 forbids scanning it and `silent-sync` enforces that.
#[test]
fn segwit_v2_spending_transaction_is_skipped_although_upstream_would_match() {
    let txs = fixture_txs(6);
    let (tx, prevouts) = &txs[2]; // coinbase, P2WSH-only, **SegWit v2**, no-taproot
    assert!(prevouts.iter().any(|p| p
        .script_pubkey
        .witness_version()
        .is_some_and(|v| v.to_num() == 2)));

    let (scan_sk, spend_pk) = fixture_gen::vector_raw_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    let upstream = Scanner::new(scan_sk, spend_pk, BTreeMap::new());
    let upstream_matches = upstream.scan_tx(tx, prevouts).unwrap();
    assert_eq!(upstream_matches.len(), 1, "fixture must exercise the rule");

    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();
    assert_eq!(
        scanner.scan_tx(tx, prevouts).unwrap(),
        TxScanOutcome::NotEligible(SkipReason::SpendsUnknownSegwitVersion)
    );
}

#[test]
fn eligibility_reasons_for_the_non_scannable_fixture_transactions() {
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();
    let reasons: Vec<_> = fixture_txs(6)
        .iter()
        .map(|(tx, prevouts)| scanner.scan_tx(tx, prevouts).unwrap())
        .collect();
    assert_eq!(
        reasons,
        vec![
            TxScanOutcome::NotEligible(SkipReason::Coinbase),
            TxScanOutcome::NotEligible(SkipReason::NoEligibleInputs),
            TxScanOutcome::NotEligible(SkipReason::SpendsUnknownSegwitVersion),
            TxScanOutcome::NotEligible(SkipReason::NoTaprootOutputs),
        ]
    );
}

/// D-007: upstream zips inputs with prevouts and would silently ignore a
/// missing prevout; `silent-sync` refuses instead.
#[test]
fn missing_prevouts_are_an_error_not_a_silent_miss() {
    let txs = fixture_txs(3);
    let (tx, prevouts) = &txs[1];
    assert_eq!(tx.input.len(), 2);
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();
    assert_eq!(
        scanner.scan_tx(tx, &prevouts[..1]),
        Err(ProtocolError::PrevoutCountMismatch {
            inputs: 2,
            prevouts: 1
        })
    );
    let mut too_many = prevouts.clone();
    too_many.push(prevouts[0].clone());
    assert!(matches!(
        scanner.scan_tx(tx, &too_many),
        Err(ProtocolError::PrevoutCountMismatch { .. })
    ));
}

/// Sources are not consensus-validated here, so malformed witness data can
/// reach upstream code that indexes into it. That must surface as an error.
#[test]
fn malformed_taproot_witness_is_contained_as_an_error() {
    let key = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798"
        .parse()
        .unwrap();
    let p2tr = ScriptBuf::new_p2tr_tweaked(TweakedPublicKey::dangerous_assume_tweaked(key));
    let tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(fixture_txs(2)[1].0.compute_txid(), 7),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            // "Script-path spend" whose control block is only two bytes long.
            witness: Witness::from_slice(&[vec![1u8, 2, 3], vec![4u8, 5]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(1_000),
            script_pubkey: p2tr.clone(),
        }],
    };
    let prevouts = [TxOut {
        value: Amount::from_sat(2_000),
        script_pubkey: p2tr,
    }];
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();
    assert_eq!(
        scanner.scan_tx(&tx, &prevouts),
        Err(ProtocolError::UpstreamPanic)
    );
    assert_eq!(
        ReceiveScanner::public_tweak_data(&tx, &prevouts),
        Err(ProtocolError::UpstreamPanic)
    );
}

// ---------------------------------------------------------------------------
// Fixture adapter validation
// ---------------------------------------------------------------------------

fn fixture_file() -> FixtureFile {
    serde_json::from_str(FIXTURE_JSON).unwrap()
}

fn load_err(file: FixtureFile) -> String {
    match FixtureSource::from_file(file) {
        Err(SourceError::InvalidData(msg)) => msg,
        other => panic!("expected InvalidData, got {other:?}"),
    }
}

fn edit_block(file: &mut FixtureFile, index: usize, edit: impl FnOnce(&mut Block)) {
    let mut block: Block = deserialize_hex(&file.blocks[index].block_hex).unwrap();
    edit(&mut block);
    file.blocks[index].block_hex = serialize_hex(&block);
}

#[test]
fn fixture_adapter_reports_what_it_does_and_does_not_verify() {
    let caps: SourceCapabilities = source().capabilities();
    assert!(caps.verifies_merkle_root && caps.verifies_witness_commitment);
    assert!(caps.verifies_proof_of_work);
    assert!(
        !caps.consensus_validated,
        "fixture txs are not consensus-valid"
    );
    assert!(!caps.remote);
    assert_eq!(source().tip().unwrap().height, TIP);
}

#[test]
fn fixture_adapter_refuses_mainnet() {
    let json = FIXTURE_JSON.replacen("\"network\": \"regtest\"", "\"network\": \"mainnet\"", 1);
    assert_ne!(json, FIXTURE_JSON);
    match FixtureSource::from_json_str(&json) {
        Err(SourceError::InvalidData(msg)) => assert!(msg.contains("mainnet is refused"), "{msg}"),
        other => panic!("mainnet fixture must be refused, got {other:?}"),
    }

    // A mainnet genesis hash under a regtest label is refused as well.
    let mut file = fixture_file();
    file.genesis_hash = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        .parse()
        .unwrap();
    assert!(load_err(file).contains("genesis hash mismatch"));

    // Claiming signet for regtest data fails on the genesis hash too.
    let mut file = fixture_file();
    file.network = Network::Signet;
    assert!(load_err(file).contains("genesis hash mismatch"));
}

#[test]
fn fixture_adapter_rejects_tampered_data() {
    // Changed output value: txid changes, merkle root no longer matches.
    let mut file = fixture_file();
    edit_block(&mut file, 1, |b| {
        b.txdata[1].output[0].value = Amount::from_sat(49_999)
    });
    assert!(load_err(file).contains("merkle root"));

    // Changed witness only: txids and merkle root still match, but the
    // witness commitment does not. Witness data is what BIP352 reads.
    let mut file = fixture_file();
    edit_block(&mut file, 1, |b| {
        let pk = b.txdata[1].input[0].witness.last().unwrap().to_vec();
        b.txdata[1].input[0].witness = Witness::from_slice(&[vec![0x31; 71], pk]);
    });
    assert!(load_err(file).contains("witness commitment"));

    // Removed transaction.
    let mut file = fixture_file();
    edit_block(&mut file, 2, |b| {
        b.txdata.pop();
    });
    assert!(load_err(file).contains("merkle root"));

    // Header of a middle block changed but still valid on its own: the next
    // block no longer links to it.
    let mut file = fixture_file();
    edit_block(&mut file, 3, |b| {
        b.header.time += 1;
        fixture_gen::seal(b);
    });
    assert!(load_err(file).contains("block 5: prev_blockhash"));

    // Blocks swapped / missing / mis-numbered.
    let mut file = fixture_file();
    file.blocks.swap(4, 5);
    assert!(load_err(file).contains("heights must be contiguous"));
    let mut file = fixture_file();
    file.blocks.remove(2);
    assert!(load_err(file).contains("heights must be contiguous"));
    let mut file = fixture_file();
    file.blocks.remove(0);
    assert!(load_err(file).contains("heights must be contiguous"));

    // Right numbering, wrong parent.
    let mut file = fixture_file();
    let moved = file.blocks[4].block_hex.clone();
    file.blocks[5].block_hex = moved;
    assert!(load_err(file).contains("does not link"));

    // Prevouts missing for one input / for a whole transaction.
    let mut file = fixture_file();
    file.blocks[2].prevouts[1].pop();
    assert!(load_err(file).contains("needing prevouts"));
    let mut file = fixture_file();
    file.blocks[2].prevouts.pop();
    assert!(load_err(file).contains("prevout lists"));

    // Unknown format / unknown fields / empty.
    let mut file = fixture_file();
    file.format = "silent-sync-lab/fixture-chain/v0".to_string();
    assert!(load_err(file).contains("unsupported fixture format"));
    let json = FIXTURE_JSON.replacen("\"format\"", "\"surprise\": 1, \"format\"", 1);
    assert!(matches!(
        FixtureSource::from_json_str(&json),
        Err(SourceError::InvalidData(_))
    ));
    let mut file = fixture_file();
    file.blocks.clear();
    assert!(load_err(file).contains("no blocks"));
}

#[test]
fn fixture_adapter_never_returns_a_partial_range() {
    let src = source();
    assert_eq!(
        src.fetch_range(5, TIP + 1),
        Err(SourceError::RangeUnavailable {
            from: 5,
            to: TIP + 1,
            available_from: 1,
            available_to: TIP
        })
    );
    assert!(matches!(
        src.fetch_range(0, 3),
        Err(SourceError::RangeUnavailable { .. })
    ));
    assert_eq!(
        src.fetch_range(4, 3),
        Err(SourceError::InvalidRange { from: 4, to: 3 })
    );
    assert_eq!(src.fetch_range(3, 5).unwrap().blocks.len(), 3);
}

// ---------------------------------------------------------------------------
// Engine: fail-closed against a misbehaving source
// ---------------------------------------------------------------------------

/// A source that serves the fixture after applying an arbitrary corruption.
struct Misbehaving<F: Fn(&mut ScanBatch)> {
    inner: FixtureSource,
    chain: ChainIdentity,
    corrupt: F,
}

impl<F: Fn(&mut ScanBatch)> ScanSource for Misbehaving<F> {
    fn backend_id(&self) -> &str {
        "misbehaving"
    }
    fn capabilities(&self) -> SourceCapabilities {
        self.inner.capabilities()
    }
    fn chain(&self) -> ChainIdentity {
        self.chain
    }
    fn tip(&self) -> Result<BlockRef, SourceError> {
        self.inner.tip()
    }
    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        let mut batch = self.inner.fetch_range(from, to)?;
        (self.corrupt)(&mut batch);
        Ok(batch)
    }
}

fn scan_misbehaving(corrupt: impl Fn(&mut ScanBatch)) -> Result<ScanReport, ScanError> {
    let src = Misbehaving {
        inner: source(),
        chain: regtest(),
        corrupt,
    };
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap();
    scan_range(&src, &scanner, regtest(), 1, TIP)
}

fn assert_inconsistent(result: Result<ScanReport, ScanError>, needle: &str) {
    match result {
        Err(ScanError::InconsistentBatch { reason, .. }) => {
            assert!(
                reason.contains(needle),
                "`{reason}` should mention `{needle}`"
            )
        }
        other => panic!("expected InconsistentBatch({needle}), got {other:?}"),
    }
}

#[test]
fn engine_accepts_an_honest_wrapped_source() {
    let report = scan_misbehaving(|_| {}).unwrap();
    assert_eq!(report.detected_received_outputs.len(), 7);
    assert_eq!(report.backend_id, "misbehaving");
}

#[test]
fn engine_rejects_short_reordered_duplicated_or_unlinked_batches() {
    // Omitted block (a server dropping the block that pays us).
    assert_inconsistent(
        scan_misbehaving(|b| {
            b.blocks.remove(1);
        }),
        "requested 8 blocks",
    );
    // Truncated tail.
    assert_inconsistent(
        scan_misbehaving(|b| b.blocks.truncate(5)),
        "requested 8 blocks",
    );
    // Empty answer.
    assert_inconsistent(scan_misbehaving(|b| b.blocks.clear()), "requested 8 blocks");
    // Reordered.
    assert_inconsistent(
        scan_misbehaving(|b| b.blocks.swap(2, 3)),
        "position 2 holds height 4",
    );
    // Duplicated block in place of another (count stays right).
    assert_inconsistent(
        scan_misbehaving(|b| b.blocks[4] = b.blocks[3].clone()),
        "position 4 holds height 4",
    );
    // Right heights, broken hash link (block from another chain spliced in).
    assert_inconsistent(
        scan_misbehaving(|b| b.blocks[3].block.prev_hash = b.blocks[0].block.hash),
        "does not link",
    );
    // Extra block.
    assert_inconsistent(
        scan_misbehaving(|b| {
            let last = b.blocks[7].clone();
            b.blocks.push(last);
        }),
        "requested 8 blocks",
    );
}

/// What the engine can NOT detect on its own: a source that drops a single
/// transaction from otherwise well-formed block evidence. The batch is still
/// structurally consistent, so the payment is silently missed. Only the source
/// adapter (merkle root, as the fixture adapter does) or a second source can
/// catch this. Recorded here so the limitation stays visible (design spec §5.5).
#[test]
fn engine_alone_cannot_detect_an_omitted_transaction() {
    let report = scan_misbehaving(|b| {
        let Evidence::FullBlock(txs) = &mut b.blocks[1].evidence else {
            panic!("fixture always yields full blocks");
        };
        txs.remove(1);
    })
    .unwrap();
    assert_eq!(
        report.detected_received_outputs.len(),
        6,
        "one payment silently lost"
    );
}

#[test]
fn engine_propagates_transaction_errors_and_refuses_other_chains() {
    // Prevouts stripped from one transaction: whole range fails, with context.
    match scan_misbehaving(|b| {
        let Evidence::FullBlock(txs) = &mut b.blocks[2].evidence else {
            panic!("fixture always yields full blocks");
        };
        txs[1].prevouts.pop();
    }) {
        Err(ScanError::Transaction {
            height: 3, source, ..
        }) => {
            assert!(matches!(source, ProtocolError::PrevoutCountMismatch { .. }))
        }
        other => panic!("expected a transaction error, got {other:?}"),
    }

    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).unwrap();

    // The wallet is configured for signet, the source serves regtest.
    let signet = ChainIdentity::for_network(Network::Signet);
    assert!(matches!(
        scan_range(&source(), &scanner, signet, 1, TIP),
        Err(ScanError::ChainMismatch { .. })
    ));

    // The source claims another chain than the wallet expects.
    let lying = Misbehaving {
        inner: source(),
        chain: signet,
        corrupt: |_: &mut ScanBatch| {},
    };
    assert!(matches!(
        scan_range(&lying, &scanner, regtest(), 1, TIP),
        Err(ScanError::ChainMismatch { .. })
    ));

    assert!(matches!(
        scan_range(&source(), &scanner, regtest(), 5, 4),
        Err(ScanError::InvalidRange { from: 5, to: 4 })
    ));
    assert!(matches!(
        scan_range(&source(), &scanner, regtest(), 1, TIP + 1),
        Err(ScanError::Source(SourceError::RangeUnavailable { .. }))
    ));
}

// ---------------------------------------------------------------------------
// Secrets must not leak into reports or debug output
// ---------------------------------------------------------------------------

#[test]
fn reports_and_debug_output_contain_no_key_material() {
    let (scan_sk, spend_pk) = fixture_gen::vector_raw_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    let keys = ReceiverKeys::new(scan_sk, spend_pk);
    let scanner = ReceiveScanner::new(&keys, &[LABEL_USED, LABEL_NOT_BACKED_UP]).unwrap();
    let report = scan_all(&scanner);
    assert_eq!(report.detected_received_outputs.len(), 8);

    let mut secrets = vec![scan_sk.display_secret().to_string()];
    // Per-output tweaks, obtained from upstream directly: they must be dropped.
    for height in 1..=TIP {
        for (tx, prevouts) in fixture_txs(height) {
            if let Ok(TxScanOutcome::Scanned { matches }) = scanner.scan_tx(&tx, &prevouts) {
                secrets.extend(
                    matches
                        .iter()
                        .map(|m| m.sensitive_output_tweak().display_secret().to_string()),
                );
            }
        }
    }
    assert_eq!(secrets.len(), 9, "scan key + 8 output tweaks");

    // `ScanMatch` carries the tweak but must not show it.
    let (tx, prevouts) = &fixture_txs(5)[1];
    let outcome = scanner.scan_tx(tx, prevouts).unwrap();
    assert!(matches!(&outcome, TxScanOutcome::Scanned { matches } if matches.len() == 3));

    let outputs = [
        serde_json::to_string(&report).unwrap(),
        format!("{report:?}"),
        format!("{keys:?}"),
        format!("{scanner:?}"),
        format!("{outcome:?}"),
        format!(
            "{} {:?}",
            keys.address(Network::Regtest),
            keys.address(Network::Signet)
        ),
    ];
    for text in &outputs {
        let lower = text.to_lowercase();
        for secret in &secrets {
            assert!(!lower.contains(secret), "secret material leaked: {text}");
        }
    }
    assert!(outputs[2].contains("<redacted>") && outputs[3].contains("<redacted>"));
    // A match shows which output it is and nothing about the tweak.
    assert_eq!(outputs[4].matches("ScanMatch {").count(), 3);
    assert!(!outputs[4].contains("tweak"));
    // Addresses are public, and can only be rendered for a lab network.
    assert!(outputs[5].starts_with("sprt1") && outputs[5].contains("SilentPaymentAddress(tsp1"));
}

// ---------------------------------------------------------------------------
// Witness-stripped data (review finding: must be an error, never a silent miss)
// ---------------------------------------------------------------------------

fn strip_witnesses(block: &mut Block, include_coinbase: bool) {
    let skip = usize::from(!include_coinbase);
    for tx in block.txdata.iter_mut().skip(skip) {
        for txin in &mut tx.input {
            txin.witness = Witness::new();
        }
    }
}

/// Stripping witnesses leaves txids, the merkle root, the block hash and the
/// proof of work untouched. rust-bitcoin's own commitment check accepts such a
/// block; the adapter must not, because every P2WPKH / P2TR payment in it
/// would become invisible.
#[test]
fn fixture_adapter_rejects_witness_stripped_blocks() {
    // Sanity: the stripped block really is indistinguishable by hash.
    let original: Block = deserialize_hex(&fixture_file().blocks[1].block_hex).unwrap();
    let mut stripped = original.clone();
    strip_witnesses(&mut stripped, true);
    assert_eq!(stripped.block_hash(), original.block_hash());
    assert!(stripped.check_merkle_root());
    assert!(
        stripped.check_witness_commitment(),
        "the upstream check is lenient"
    );

    // Every block fully stripped.
    let mut file = fixture_file();
    for index in 0..file.blocks.len() {
        edit_block(&mut file, index, |b| strip_witnesses(b, true));
    }
    assert!(load_err(file).contains("block 1: witness commitment"));

    // Only the block holding the first payment, fully stripped.
    let mut file = fixture_file();
    edit_block(&mut file, 1, |b| strip_witnesses(b, true));
    let msg = load_err(file);
    assert!(
        msg.contains("block 2: witness commitment") && msg.contains("reserved value"),
        "{msg}"
    );

    // Transactions stripped, coinbase reserved value kept.
    let mut file = fixture_file();
    edit_block(&mut file, 1, |b| strip_witnesses(b, false));
    let msg = load_err(file);
    assert!(
        msg.contains("block 2: witness commitment") && msg.contains("does not match"),
        "{msg}"
    );

    // Witness data present but the coinbase commits to nothing.
    let mut file = fixture_file();
    edit_block(&mut file, 7, |b| {
        b.txdata[0].output.truncate(1);
        b.header.merkle_root = b.compute_merkle_root().unwrap();
        b.header.nonce = 0;
        while b.header.validate_pow(b.header.target()).is_err() {
            b.header.nonce += 1;
        }
    });
    let msg = load_err(file);
    assert!(
        msg.contains("block 8: witness commitment") && msg.contains("no commitment"),
        "{msg}"
    );

    // A block without any witness data and without a commitment is fine.
    let mut file = fixture_file();
    file.blocks.truncate(1);
    edit_block(&mut file, 0, |b| {
        b.txdata[0].output.truncate(1);
        b.txdata[0].input[0].witness = Witness::new();
        b.header.merkle_root = b.compute_merkle_root().unwrap();
        b.header.nonce = 0;
        while b.header.validate_pow(b.header.target()).is_err() {
            b.header.nonce += 1;
        }
    });
    assert!(FixtureSource::from_file(file).is_ok());
}

/// The same protection one layer down, for sources that do not (or cannot)
/// check a witness commitment.
#[test]
fn scanner_refuses_segwit_spends_without_witness() {
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap();
    let stripped = |height: u32, index: usize, inputs: &[usize]| {
        let (mut tx, prevouts) = fixture_txs(height)[index].clone();
        for &i in inputs {
            tx.input[i].witness = Witness::new();
        }
        scanner.scan_tx(&tx, &prevouts)
    };
    let missing = |input| Err(ProtocolError::MissingWitness { input });

    assert_eq!(stripped(2, 1, &[0]), missing(0), "P2WPKH");
    assert_eq!(stripped(5, 1, &[0]), missing(0), "P2TR");
    assert_eq!(stripped(3, 1, &[1]), missing(1), "second input only");
    assert_eq!(stripped(3, 1, &[0, 1]), missing(0));
    assert_eq!(stripped(6, 1, &[0]), missing(0), "P2WSH");
    // Reported even when the transaction would be skipped anyway.
    assert_eq!(stripped(6, 3, &[0]), missing(0), "no taproot output");
    // Unmodified transactions are unaffected.
    assert!(
        matches!(stripped(2, 1, &[]), Ok(TxScanOutcome::Scanned { matches }) if matches.len() == 1)
    );

    // Hand-built cases around the rule's edges.
    let p2tr_out = fixture_txs(2)[1].0.output[0].clone();
    let spend = |prevout_script: ScriptBuf, script_sig: ScriptBuf| {
        let tx = Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::new(fixture_txs(2)[1].0.compute_txid(), 0),
                script_sig,
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![p2tr_out.clone()],
        };
        let prevouts = [TxOut {
            value: Amount::from_sat(1_000),
            script_pubkey: prevout_script,
        }];
        scanner.scan_tx(&tx, &prevouts)
    };
    let p2sh = ScriptBuf::from_hex("a914809b71783f1b55eeadeb1678baef0c994adc425987").unwrap();
    let not_eligible = Ok(TxScanOutcome::NotEligible(SkipReason::NoEligibleInputs));

    // P2SH-P2WPKH: scriptSig is exactly one push of a v0 witness program.
    let wrapped = ScriptBuf::from_hex("1600146a721dcca372f3c17b2c649b2ba61aa0fda98a91").unwrap();
    assert_eq!(spend(p2sh.clone(), wrapped), missing(0));
    // Legacy P2SH: a scriptSig with more than that single push needs no
    // witness, wherever the program-shaped push sits.
    let program = "1600146a721dcca372f3c17b2c649b2ba61aa0fda98a91";
    for script_sig in [format!("0101{program}"), format!("{program}0151")] {
        let legacy = ScriptBuf::from_hex(&script_sig).unwrap();
        assert_eq!(spend(p2sh.clone(), legacy), not_eligible, "{script_sig}");
    }
    // Pay-to-anchor (v1, 2-byte program) is spent with an empty witness.
    let anchor = ScriptBuf::from_hex("51024e73").unwrap();
    assert_eq!(spend(anchor, ScriptBuf::new()), not_eligible);
    // Bare / unknown scripts carry no witness either.
    let bare = Builder::new().push_int(1).into_script();
    assert_eq!(spend(bare, ScriptBuf::new()), not_eligible);
}

#[test]
fn engine_fails_the_range_when_a_source_serves_stripped_transactions() {
    let result = scan_misbehaving(|b| {
        let Evidence::FullBlock(txs) = &mut b.blocks[1].evidence else {
            panic!("fixture always yields full blocks");
        };
        txs[1].tx.input[0].witness = Witness::new();
    });
    match result {
        Err(ScanError::Transaction {
            height: 2, source, ..
        }) => {
            assert_eq!(source, ProtocolError::MissingWitness { input: 0 })
        }
        other => panic!("expected MissingWitness at height 2, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Fixture adapter: each structural check has a test that fails without it
// ---------------------------------------------------------------------------

/// Index of the tip block in the fixture file; editing it breaks no later link.
const TIP_INDEX: usize = 7;

#[test]
fn fixture_adapter_checks_proof_of_work() {
    // Valid structure, but a nonce that does not meet the target.
    let mut file = fixture_file();
    edit_block(&mut file, TIP_INDEX, |b| {
        while b.header.validate_pow(b.header.target()).is_ok() {
            b.header.nonce += 1;
        }
    });
    let msg = load_err(file);
    assert!(msg.contains("block 8: proof of work check failed"), "{msg}");

    // `bits` chosen by the file must be canonical and within the regtest limit.
    for (bits, needle) in [
        (0x227f_ffff_u32, "not a canonical target encoding"), // overflows 256 bits
        (0xff7f_ffff, "not a canonical target encoding"),
        (0x2100_8000, "easier than the regtest limit"), // 2^255
        (0x2080_0000, "not a canonical target encoding"), // negative
    ] {
        let mut file = fixture_file();
        edit_block(&mut file, TIP_INDEX, |b| {
            b.header.bits = CompactTarget::from_consensus(bits);
            b.header.nonce = 0;
        });
        let msg = load_err(file);
        assert!(
            msg.contains("block 8: proof of work check failed") && msg.contains(needle),
            "{bits:#x}: {msg}"
        );
    }
}

#[test]
fn fixture_adapter_checks_the_coinbase_height() {
    let with_script_sig = |script_sig: ScriptBuf| {
        let mut file = fixture_file();
        edit_block(&mut file, TIP_INDEX, |b| {
            b.txdata[0].input[0].script_sig = script_sig;
            fixture_gen::seal(b);
        });
        load_err(file)
    };
    let wrong = Builder::new().push_int(9).push_slice(b"x").into_script();
    assert!(with_script_sig(wrong).contains("block 8: coinbase commits to height 9"));
    let none = Builder::new().push_slice(b"no height here").into_script();
    assert!(with_script_sig(none).contains("block 8: coinbase does not commit to a height"));
}

#[test]
fn fixture_adapter_checks_coinbase_position_and_duplicates() {
    // Coinbase not first.
    let mut file = fixture_file();
    file.blocks[TIP_INDEX].prevouts.swap(0, 1);
    edit_block(&mut file, TIP_INDEX, |b| {
        b.txdata.swap(0, 1);
        b.header.merkle_root = b.compute_merkle_root().unwrap();
        b.header.nonce = 0;
        while b.header.validate_pow(b.header.target()).is_err() {
            b.header.nonce += 1;
        }
    });
    let msg = load_err(file);
    assert!(msg.contains("block 8: transaction 0 violates"), "{msg}");

    // A second coinbase further down.
    let mut file = fixture_file();
    file.blocks[TIP_INDEX].prevouts.push(vec![]);
    edit_block(&mut file, TIP_INDEX, |b| {
        let mut second = b.txdata[0].clone();
        second.input[0].sequence = Sequence::ZERO;
        b.txdata.push(second);
        fixture_gen::seal(b);
    });
    let msg = load_err(file);
    assert!(msg.contains("block 8: transaction 2 violates"), "{msg}");

    // The same transaction twice in one block.
    let mut file = fixture_file();
    let prevouts = file.blocks[TIP_INDEX].prevouts[1].clone();
    file.blocks[TIP_INDEX].prevouts.push(prevouts);
    edit_block(&mut file, TIP_INDEX, |b| {
        let again = b.txdata[1].clone();
        b.txdata.push(again);
        fixture_gen::seal(b);
    });
    let msg = load_err(file);
    assert!(msg.contains("block 8: duplicate transaction"), "{msg}");

    // A transaction of an earlier block repeated in a later one.
    let mut file = fixture_file();
    let earlier: Block = deserialize_hex(&file.blocks[1].block_hex).unwrap();
    let prevouts = file.blocks[1].prevouts[1].clone();
    file.blocks[TIP_INDEX].prevouts.push(prevouts);
    edit_block(&mut file, TIP_INDEX, |b| {
        b.txdata.push(earlier.txdata[1].clone());
        fixture_gen::seal(b);
    });
    let msg = load_err(file);
    assert!(msg.contains("block 8: duplicate transaction"), "{msg}");
}

/// The `genesis_hash` field is only a claim; what binds the blocks to the
/// claimed network is that block 1 actually builds on that genesis block.
#[test]
fn fixture_adapter_requires_block_one_to_build_on_the_network_genesis() {
    let mut file = fixture_file();
    let mut parent = Network::Signet.genesis_hash();
    for index in 0..file.blocks.len() {
        edit_block(&mut file, index, |b| {
            b.header.prev_blockhash = parent;
            fixture_gen::seal(b);
            parent = b.block_hash();
        });
    }
    // The chain is internally consistent (every block links to the previous
    // one) but hangs off another network's genesis block.
    let msg = load_err(file);
    assert!(
        msg.contains("block 1: prev_blockhash") && msg.contains("does not link"),
        "{msg}"
    );
}

// ---------------------------------------------------------------------------
// Engine: structural block checks independent of the source
// ---------------------------------------------------------------------------

fn full_block(
    batch: &mut ScanBatch,
    index: usize,
) -> &mut Vec<silent_sync::source::TxWithPrevouts> {
    let Evidence::FullBlock(txs) = &mut batch.blocks[index].evidence else {
        panic!("fixture always yields full blocks");
    };
    txs
}

#[test]
fn engine_rejects_structurally_impossible_blocks() {
    // A block without any transaction (e.g. a source answering with empty lists).
    assert_inconsistent(
        scan_misbehaving(|b| full_block(b, 3).clear()),
        "block 4 has no transactions",
    );
    assert_inconsistent(
        scan_misbehaving(|b| {
            for index in 0..b.blocks.len() {
                full_block(b, index).clear();
            }
        }),
        "block 1 has no transactions",
    );
    // Coinbase missing.
    assert_inconsistent(
        scan_misbehaving(|b| {
            full_block(b, 1).remove(0);
        }),
        "block 2: transaction at position 0 violates",
    );
    // Whole transaction list duplicated: a second coinbase appears.
    assert_inconsistent(
        scan_misbehaving(|b| {
            let txs = full_block(b, 2);
            let copy = txs.clone();
            txs.extend(copy);
        }),
        "block 3: transaction at position 3 violates",
    );
    // One transaction repeated inside a block.
    assert_inconsistent(
        scan_misbehaving(|b| {
            let txs = full_block(b, 1);
            let copy = txs[1].clone();
            txs.push(copy);
        }),
        "appears more than once",
    );
    // A transaction repeated in a later block (would be counted twice).
    assert_inconsistent(
        scan_misbehaving(|b| {
            let copy = full_block(b, 1)[1].clone();
            full_block(b, 4).push(copy);
        }),
        "block 5: transaction 391d63ac72c9c60e7cb066ad829aadd8cf201e4fff3ad07589d7f705cde04b11 appears more than once",
    );
    // The first block of a batch has no predecessor to link to, but it can
    // still be caught claiming to be its own parent.
    assert_inconsistent(
        scan_misbehaving(|b| b.blocks[0].block.prev_hash = b.blocks[0].block.hash),
        "block 1 claims to be its own parent",
    );
}

/// Recomputes the merkle root and regtest proof of work after editing a block
/// by hand (unlike `fixture_gen::seal`, the witness commitment is left alone).
fn remine(block: &mut Block) {
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block.header.nonce = 0;
    while block.header.validate_pow(block.header.target()).is_err() {
        block.header.nonce += 1;
    }
}

/// BIP141 details of locating and checking the witness commitment.
#[test]
fn witness_commitment_location_rules() {
    let is_commitment = |o: &TxOut| {
        o.script_pubkey
            .as_bytes()
            .starts_with(&[0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed])
    };
    let bogus = || {
        let mut script = vec![0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
        script.extend_from_slice(&[0x77; 32]);
        TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::from_bytes(script),
        }
    };
    let load = |edit: &dyn Fn(&mut Block)| {
        let mut file = fixture_file();
        edit_block(&mut file, TIP_INDEX, |b| {
            edit(b);
            remine(b);
        });
        FixtureSource::from_file(file)
    };

    // Sanity: the tip block has witness data and exactly one commitment.
    let tip: Block = deserialize_hex(&fixture_file().blocks[TIP_INDEX].block_hex).unwrap();
    assert_eq!(
        tip.txdata[0]
            .output
            .iter()
            .filter(|o| is_commitment(o))
            .count(),
        1
    );
    assert!(is_commitment(tip.txdata[0].output.last().unwrap()));

    // Several commitment-shaped outputs: the *last* one counts.
    assert!(load(&|b| {
        let at = b.txdata[0].output.len() - 1;
        b.txdata[0].output.insert(at, bogus());
    })
    .is_ok());
    match load(&|b| b.txdata[0].output.push(bogus())) {
        Err(SourceError::InvalidData(msg)) => assert!(
            msg.contains("does not match the coinbase commitment"),
            "{msg}"
        ),
        other => panic!("a wrong last commitment must be refused, got {other:?}"),
    }

    // A commitment script may be longer than the 38 bytes that matter.
    assert!(load(&|b| {
        let last = b.txdata[0].output.last_mut().unwrap();
        let mut script = last.script_pubkey.to_bytes();
        script.push(0x51);
        last.script_pubkey = ScriptBuf::from_bytes(script);
    })
    .is_ok());

    // The coinbase witness must be exactly one 32-byte item.
    for witness in [
        Witness::from_slice(&[[0u8; 32], [0u8; 32]]),
        Witness::from_slice(&[vec![0u8; 31]]),
        Witness::from_slice(&[vec![0u8; 33]]),
    ] {
        match load(&|b| b.txdata[0].input[0].witness = witness.clone()) {
            Err(SourceError::InvalidData(msg)) => assert!(msg.contains("reserved value"), "{msg}"),
            other => panic!("a malformed reserved value must be refused, got {other:?}"),
        }
    }
    // A different (well-formed) reserved value no longer matches.
    assert!(load(&|b| b.txdata[0].input[0].witness = Witness::from_slice(&[[1u8; 32]])).is_err());
}

/// The fixture adapter cross-checks prevouts of inputs that spend an output
/// created inside the file (it cannot check any other prevout).
#[test]
fn fixture_adapter_cross_checks_prevouts_created_in_the_file() {
    // Append a transaction to the tip block that spends the first payment
    // (created at height 2), with a placeholder witness.
    let payment = fixture_txs(2)[1].0.clone();
    let spent = OutPoint::new(payment.compute_txid(), 0);
    let real = payment.output[0].clone();
    let spender = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: spent,
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![0u8; 64]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(40_000),
            script_pubkey: ScriptBuf::from_hex("0014aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
                .unwrap(),
        }],
    };
    let with_prevout = |value_sats: u64, script: &ScriptBuf| {
        let mut file = fixture_file();
        file.blocks[TIP_INDEX]
            .prevouts
            .push(vec![silent_sync::source::fixture::FixturePrevout {
                value_sats,
                script_pubkey_hex: script.to_hex_string(),
            }]);
        edit_block(&mut file, TIP_INDEX, |b| {
            b.txdata.push(spender.clone());
            fixture_gen::seal(b);
        });
        FixtureSource::from_file(file)
    };

    assert!(with_prevout(real.value.to_sat(), &real.script_pubkey).is_ok());
    for (value, script) in [
        (real.value.to_sat() + 1, real.script_pubkey.clone()),
        (real.value.to_sat(), payment.output[1].script_pubkey.clone()),
    ] {
        match with_prevout(value, &script) {
            Err(SourceError::InvalidData(msg)) => {
                assert!(
                    msg.contains("differs from the output created in this range"),
                    "{msg}"
                )
            }
            other => panic!("a lying prevout must be refused, got {other:?}"),
        }
    }
}

/// `scan_range` works in chunks; the result must not depend on it.
#[test]
fn stateless_scan_is_chunked_and_still_all_or_nothing() {
    use silent_sync::engine::SCAN_CHUNK_BLOCKS;
    // Count fetches through a wrapper and check linkage across chunk borders
    // with a source that serves the fork from the second chunk on.
    struct Counting<'a>(&'a FixtureSource, std::cell::Cell<u32>);
    impl ScanSource for Counting<'_> {
        fn backend_id(&self) -> &str {
            "counting"
        }
        fn capabilities(&self) -> SourceCapabilities {
            self.0.capabilities()
        }
        fn chain(&self) -> ChainIdentity {
            self.0.chain()
        }
        fn tip(&self) -> Result<BlockRef, SourceError> {
            self.0.tip()
        }
        fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
            self.1.set(self.1.get() + 1);
            assert!(
                to - from < SCAN_CHUNK_BLOCKS,
                "a chunk larger than the limit was requested"
            );
            self.0.fetch_range(from, to)
        }
    }
    let src = source();
    let counting = Counting(&src, std::cell::Cell::new(0));
    let scanner = ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap();
    let report = scan_range(&counting, &scanner, regtest(), 1, TIP).unwrap();
    assert_eq!(report.detected_received_outputs.len(), 7);
    assert_eq!(counting.1.get(), TIP.div_ceil(SCAN_CHUNK_BLOCKS));
}
