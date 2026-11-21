//! Gate 4 — a second, independent kind of evidence: the tweak index.
//!
//! * A locally computed index (BIP352 policy) must give exactly the result of
//!   scanning full blocks.
//! * An index that ignores the "spends SegWit version > 1" rule (as BlindBit
//!   Oracle v2 was observed to) gives one extra detection, and `cross_check`
//!   explains it per transaction.
//! * What an index can do to a client that trusts it — omit, alter, invent —
//!   and which of that a cross-check against full blocks exposes.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LABEL_NOT_BACKED_UP, LABEL_USED},
    regtest_wallet,
    test_source::{Tamper, TestSource},
};
use sync_core::{
    bitcoin::{hashes::Hash, Txid},
    engine::{scan_range, ScanError},
    models::{DetectedOutput, EvidenceKind},
    network::{ChainIdentity, LabNetwork},
    protocol::{ProtocolError, ReceiveScanner},
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
        Evidence, IndexedOutput, IndexedTx, ScanBatch, ScanSource,
    },
    store::{MemoryStore, SyncStore},
    sync::{new_store_meta, sync, SyncOptions},
    verify::{cross_check, Explanation, Side},
};

const TIP: u32 = 8;

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(LabNetwork::Regtest)
}

fn scanner_with(labels: &[u32]) -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), labels).unwrap()
}

fn scanner() -> ReceiveScanner {
    scanner_with(&[LABEL_USED])
}

fn full_blocks() -> FixtureSource {
    FixtureSource::from_file(fixture_gen::generate()).unwrap()
}

fn index(policy: IndexPolicy) -> LocalTweakIndex<FixtureSource> {
    LocalTweakIndex::new(full_blocks(), policy)
}

/// Detections without provenance (which necessarily differs between backends).
fn outputs(detections: &[DetectedOutput]) -> Vec<(u32, String, u32, u64, Option<u32>)> {
    detections
        .iter()
        .map(|d| {
            (
                d.block.height,
                d.txid.to_string(),
                d.vout,
                d.value_sats,
                d.label,
            )
        })
        .collect()
}

fn indexed(batch: &mut ScanBatch, block_index: usize) -> &mut Vec<IndexedTx> {
    let Evidence::TweakIndex(txs) = &mut batch.blocks[block_index].evidence else {
        panic!("tweak-index evidence expected");
    };
    txs
}

#[test]
fn a_bip352_index_gives_exactly_the_full_block_result() {
    let blocks = full_blocks();
    let index = index(IndexPolicy::Bip352);
    assert_eq!(index.backend_id(), "local_tweak_index");
    let caps = index.capabilities();
    assert_eq!(caps.evidence_kind, EvidenceKind::TweakIndex);
    assert!(
        !caps.verifies_merkle_root
            && !caps.verifies_witness_commitment
            && !caps.consensus_validated,
        "a client of a tweak index cannot check the blocks"
    );

    for labels in [
        vec![],
        vec![LABEL_USED],
        vec![LABEL_USED, LABEL_NOT_BACKED_UP],
        vec![LABEL_NOT_BACKED_UP],
    ] {
        let scanner = scanner_with(&labels);
        let via_blocks = scan_range(&blocks, &scanner, regtest(), 1, TIP).unwrap();
        let via_index = scan_range(&index, &scanner, regtest(), 1, TIP).unwrap();
        assert_eq!(
            outputs(&via_index.detected_received_outputs),
            outputs(&via_blocks.detected_received_outputs),
            "labels {labels:?}"
        );
        assert!(via_index
            .detected_received_outputs
            .iter()
            .all(|d| d.provenance.evidence_kind == EvidenceKind::TweakIndex));
        // The index lists only what BIP352 says to scan: 6 transactions.
        assert_eq!((via_index.txs_seen, via_index.txs_scanned), (6, 6));
        assert!(via_index.txs_skipped.is_empty());

        let check = cross_check(&index, &blocks, &scanner, regtest(), 1, TIP).unwrap();
        assert!(check.identical, "{check:?}");
        assert_eq!(
            check.agreed as usize,
            via_blocks.detected_received_outputs.len()
        );
        assert_eq!(
            (
                check.candidate_only,
                check.reference_only,
                check.unexplained
            ),
            (0, 0, 0)
        );
    }

    // Someone else's keys find only their own payment through the index too.
    let other = ReceiveScanner::new(&fixture_gen::other_receiver_keys(), &[LABEL_USED]).unwrap();
    let theirs = scan_range(&index, &other, regtest(), 1, TIP).unwrap();
    assert_eq!(outputs(&theirs.detected_received_outputs).len(), 1);
}

/// The persistent sync works on tweak-index evidence as well, including a reorg.
#[test]
fn persistent_sync_over_a_tweak_index() {
    let source = LocalTweakIndex::new(TestSource::new(), IndexPolicy::Bip352);
    let keys = fixture_gen::lab_receiver_keys();
    let scanner = scanner();
    let mut store = MemoryStore::new();
    store
        .initialize(&new_store_meta(
            &source,
            &scanner,
            &keys.wallet_id(),
            1,
            "test",
        ))
        .unwrap();
    let options = SyncOptions {
        chunk_size: 3,
        ..SyncOptions::default()
    };
    let report = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options).unwrap();
    assert!(report.caught_up);
    let reference = scan_range(source.inner().main_chain(), &scanner, regtest(), 1, TIP).unwrap();
    assert_eq!(
        outputs(&store.detections().unwrap()),
        outputs(&reference.detected_received_outputs)
    );

    source.inner().on_fork.set(true);
    let report = sync(&source, &scanner, &keys.wallet_id(), &mut store, &options).unwrap();
    assert_eq!(report.reorg.unwrap().fork_height, fixture_gen::FORK_HEIGHT);
    let reference = scan_range(source.inner().fork_chain(), &scanner, regtest(), 1, 9).unwrap();
    assert_eq!(
        outputs(&store.detections().unwrap()),
        outputs(&reference.detected_received_outputs)
    );
}

/// The difference observed with BlindBit Oracle v2: an index that lists a
/// transaction spending a SegWit v2 output. A client of that index detects an
/// output BIP352 says must not be scanned for — and cannot know, because it
/// never sees the inputs.
#[test]
fn an_index_ignoring_the_segwit_version_rule_differs_by_exactly_that_transaction() {
    let blocks = full_blocks();
    let lenient = index(IndexPolicy::IgnoreSegwitVersionRule);
    let scanner = scanner();

    let via_blocks = scan_range(&blocks, &scanner, regtest(), 1, TIP).unwrap();
    let via_index = scan_range(&lenient, &scanner, regtest(), 1, TIP).unwrap();
    assert_eq!(via_blocks.detected_received_outputs.len(), 7);
    assert_eq!(via_index.detected_received_outputs.len(), 8);
    assert_eq!(via_index.txs_seen, 7, "one more transaction is listed");

    let check = cross_check(&lenient, &blocks, &scanner, regtest(), 1, TIP).unwrap();
    assert!(!check.identical);
    assert_eq!(
        (check.agreed, check.candidate_only, check.reference_only),
        (7, 1, 0)
    );
    assert_eq!(check.unexplained, 0);
    let diff = &check.differences[0];
    assert_eq!(
        (diff.height, diff.vout, diff.only_in),
        (6, 0, Some(Side::Candidate))
    );
    assert_eq!(diff.candidate.as_ref().unwrap().value_sats, 60_000);
    assert_eq!(
        diff.explanation,
        Explanation::ReferenceNotEligible {
            reason: "spends_unknown_segwit_version"
        }
    );
    // The report is serializable and names the reason.
    let json = serde_json::to_string(&check).unwrap();
    assert!(
        json.contains("\"kind\":\"reference_not_eligible\"")
            && json.contains("spends_unknown_segwit_version")
    );
}

/// What a client that trusts an index is exposed to, and what a cross-check
/// against full blocks tells apart. None of these make the index-only scan
/// fail: without a second source they are silent.
#[test]
fn omissions_and_alterations_by_an_index_are_silent_until_cross_checked() {
    let blocks = full_blocks();
    let scanner = scanner();
    let check_with = |tamper: &dyn Fn(&mut ScanBatch)| {
        let lying = Tamper::new(index(IndexPolicy::Bip352), tamper);
        let alone = scan_range(&lying, &scanner, regtest(), 1, TIP).unwrap();
        let check = cross_check(&lying, &blocks, &scanner, regtest(), 1, TIP).unwrap();
        (alone.detected_received_outputs.len(), check)
    };

    // T10: one transaction (its tweak) is dropped from the block that pays us.
    let (found, check) = check_with(&|b| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 2) {
            indexed(b, i).clear();
        }
    });
    assert_eq!(
        found, 6,
        "the index-only scan succeeds and silently misses a payment"
    );
    assert_eq!(
        (check.agreed, check.reference_only, check.candidate_only),
        (6, 1, 0)
    );
    assert_eq!(
        check.differences[0].explanation,
        Explanation::CandidateOmittedTransaction
    );
    assert_eq!(check.differences[0].only_in, Some(Side::Reference));

    // The tweak of a transaction is replaced by another transaction's.
    let (found, check) = check_with(&|b| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 3) {
            let txs = indexed(b, i);
            let other = txs[1].tweak;
            txs[0].tweak = other;
        }
    });
    assert_eq!(found, 5, "both outputs of that transaction are missed");
    assert_eq!(check.reference_only, 2);
    assert!(check
        .differences
        .iter()
        .all(|d| d.explanation == Explanation::CandidateTweakDiffers));

    // One output is left out; another is listed with a different value.
    let (found, check) = check_with(&|b| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 3) {
            indexed(b, i)[0].outputs.retain(|o| o.vout != 1);
        }
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 8) {
            indexed(b, i)[0].outputs[0].value_sats = 1_000_000;
        }
    });
    // Dropping the k = 0 output hides the k = 1 output behind it as well.
    assert_eq!(found, 5);
    let explanations: Vec<_> = check
        .differences
        .iter()
        .map(|d| (d.height, d.vout, d.explanation.clone()))
        .collect();
    assert_eq!(
        explanations,
        vec![
            (3, 1, Explanation::CandidateOmittedOutput),
            (
                3,
                2,
                Explanation::Unexplained {
                    note: "the candidate lists the transaction and the output".to_string()
                }
            ),
            (8, 0, Explanation::DetailsDiffer),
        ]
    );
    assert_eq!(
        check.unexplained, 1,
        "the hidden k = 1 output has no direct explanation"
    );

    // Reordering transactions or outputs inside a block changes nothing.
    let (found, check) = check_with(&|b| {
        for i in 0..b.blocks.len() {
            let txs = indexed(b, i);
            txs.reverse();
            for tx in txs.iter_mut() {
                tx.outputs.reverse();
            }
        }
    });
    assert_eq!(found, 7);
    assert!(check.identical);
}

/// An index that knows the receiver's *address* can invent a payment: it
/// picks tweak data whose secret it knows and derives a matching output, for
/// a transaction that does not exist. The scanner has no way to notice.
/// (The entry is built in `lab-testkit`, the only place sending functions
/// are used.)
#[test]
fn an_index_can_fabricate_a_payment_and_only_a_cross_check_shows_it() {
    let blocks = full_blocks();
    let scanner = scanner();
    let receiver = fixture_gen::lab_receiver_keys().address(LabNetwork::Regtest);
    let fake_txid = Txid::from_byte_array([0x42; 32]);
    let fabricated =
        regtest_wallet::fabricated_index_entry(fake_txid, receiver, &[(0, 21_000_000)]);

    let lying = Tamper::new(index(IndexPolicy::Bip352), move |b: &mut ScanBatch| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 4) {
            indexed(b, i).push(fabricated.clone());
        }
    });
    let alone = scan_range(&lying, &scanner, regtest(), 1, TIP).unwrap();
    assert_eq!(
        alone.detected_received_outputs.len(),
        8,
        "the invented payment is 'detected'"
    );
    assert!(alone
        .detected_received_outputs
        .iter()
        .any(|d| d.txid == fake_txid && d.value_sats == 21_000_000));

    let check = cross_check(&lying, &blocks, &scanner, regtest(), 1, TIP).unwrap();
    assert_eq!((check.agreed, check.candidate_only), (7, 1));
    assert_eq!(
        check.differences[0].explanation,
        Explanation::ReferenceTransactionMissing
    );
}

#[test]
fn malformed_index_entries_are_errors() {
    let scanner = scanner();
    let lying = Tamper::new(index(IndexPolicy::Bip352), |b: &mut ScanBatch| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 2) {
            let txs = indexed(b, i);
            let again = txs[0].outputs[0].clone();
            txs[0].outputs.push(again);
        }
    });
    match scan_range(&lying, &scanner, regtest(), 1, TIP) {
        Err(ScanError::Transaction {
            height: 2,
            source: ProtocolError::Upstream(msg),
            ..
        }) => {
            assert!(msg.contains("twice"), "{msg}")
        }
        other => panic!("a duplicated output must be refused, got {other:?}"),
    }
    // The same transaction listed in two blocks.
    let lying = Tamper::new(index(IndexPolicy::Bip352), |b: &mut ScanBatch| {
        let (Some(i), Some(j)) = (
            b.blocks.iter().position(|blk| blk.block.height == 2),
            b.blocks.iter().position(|blk| blk.block.height == 4),
        ) else {
            return;
        };
        let copy = indexed(b, i)[0].clone();
        indexed(b, j).push(copy);
    });
    assert!(matches!(
        scan_range(&lying, &scanner, regtest(), 1, TIP),
        Err(ScanError::InconsistentBatch { reason, .. }) if reason.contains("more than once")
    ));
}

#[test]
fn cross_check_refuses_mismatched_chains_and_reports_block_hash_differences() {
    let scanner = scanner();
    let main = full_blocks();
    let fork = FixtureSource::from_file(fixture_gen::generate_fork()).unwrap();

    // Same network, different chains above height 5.
    let check = cross_check(
        &LocalTweakIndex::new(fork, IndexPolicy::Bip352),
        &main,
        &scanner,
        regtest(),
        1,
        TIP,
    )
    .unwrap();
    assert!(!check.identical);
    assert_eq!(check.block_hash_mismatches, vec![6, 7, 8]);
    assert_eq!(check.agreed, 5, "heights 1 to 5 are shared");
    assert!(check
        .differences
        .iter()
        .all(|d| d.height > 5 && d.explanation == Explanation::BlockHashDiffers));

    let signet = ChainIdentity::for_network(LabNetwork::Signet);
    assert!(matches!(
        cross_check(&index(IndexPolicy::Bip352), &main, &scanner, signet, 1, TIP),
        Err(ScanError::ChainMismatch { .. })
    ));
    assert!(matches!(
        cross_check(
            &index(IndexPolicy::Bip352),
            &main,
            &scanner,
            regtest(),
            5,
            4
        ),
        Err(ScanError::InvalidRange { .. })
    ));
    assert!(matches!(
        cross_check(
            &index(IndexPolicy::Bip352),
            &main,
            &scanner,
            regtest(),
            1,
            TIP + 1
        ),
        Err(ScanError::Source(_))
    ));
}

/// What the engine refuses from any index before scanning it: claims that
/// cannot be true of a block, whatever the receiver's keys are.
#[test]
fn impossible_index_claims_are_refused() {
    let scanner = scanner();
    let refused = |tamper: &dyn Fn(&mut Vec<IndexedTx>), needle: &str| {
        let lying = Tamper::new(index(IndexPolicy::Bip352), |b: &mut ScanBatch| {
            if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 2) {
                tamper(indexed(b, i));
            }
        });
        match scan_range(&lying, &scanner, regtest(), 1, TIP) {
            Err(ScanError::InconsistentBatch { reason, .. }) => {
                assert!(
                    reason.contains(needle),
                    "`{reason}` should mention `{needle}`"
                )
            }
            other => panic!("expected a refusal mentioning `{needle}`, got {other:?}"),
        }
    };
    let max_sats = sync_core::bitcoin::Amount::MAX_MONEY.to_sat();

    // An entry without outputs: an index lists a transaction for its outputs.
    refused(&|txs| txs[0].outputs.clear(), "lists no outputs");
    // More money than exists — in the output that pays us, or in any other.
    refused(
        &|txs| txs[0].outputs[0].value_sats = max_sats + 1,
        "more than can exist",
    );
    refused(
        &|txs| txs[0].outputs[1].value_sats = u64::MAX,
        "more than can exist",
    );
    // More outputs than a block can hold (bounds the work an index can cause).
    refused(
        &|txs| {
            let filler = txs[0].outputs[1].clone();
            let limit = sync_core::engine::MAX_INDEXED_OUTPUTS_PER_BLOCK as u32;
            txs[0].outputs.extend((0..limit).map(|n| IndexedOutput {
                vout: 10 + n,
                ..filler.clone()
            }));
        },
        "more than a block can hold",
    );

    // Exactly at the limits nothing is refused.
    let at_limits = Tamper::new(index(IndexPolicy::Bip352), move |b: &mut ScanBatch| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 2) {
            let txs = indexed(b, i);
            txs[0].outputs[1].value_sats = max_sats;
            let filler = txs[0].outputs[1].clone();
            let room = sync_core::engine::MAX_INDEXED_OUTPUTS_PER_BLOCK - txs[0].outputs.len();
            txs[0]
                .outputs
                .extend((0..room as u32).map(|n| IndexedOutput {
                    vout: 10 + n,
                    ..filler
                }));
        }
    });
    assert_eq!(
        scan_range(&at_limits, &scanner, regtest(), 1, TIP)
            .unwrap()
            .detected_received_outputs
            .len(),
        7
    );
}

/// The local index lists taproot outputs only, with their real positions.
#[test]
fn the_local_index_lists_only_taproot_outputs() {
    let batch = index(IndexPolicy::Bip352).fetch_range(2, 2).unwrap();
    let Evidence::TweakIndex(entries) = &batch.blocks[0].evidence else {
        panic!("index evidence expected");
    };
    // Block 2's payment: outputs 0 (silent payment) and 2 (unrelated taproot)
    // are listed; output 1 (P2WPKH change) is not.
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]
            .outputs
            .iter()
            .map(|o| o.vout)
            .collect::<Vec<_>>(),
        vec![0, 2]
    );
    assert_eq!(
        entries[0]
            .outputs
            .iter()
            .map(|o| o.value_sats)
            .collect::<Vec<_>>(),
        vec![50_000, 9_000]
    );
    // A P2WSH output is as long as a taproot one (34 bytes) and must still
    // not be listed: the payment gets such an output appended.
    let with_p2wsh = Tamper::new(full_blocks(), |b: &mut ScanBatch| {
        for block in &mut b.blocks {
            if let (2, Evidence::FullBlock(txs)) = (block.block.height, &mut block.evidence) {
                let mut script = vec![0x00, 0x20];
                script.extend([0x77; 32]);
                txs[1].tx.output.push(sync_core::bitcoin::TxOut {
                    value: sync_core::bitcoin::Amount::from_sat(1_234),
                    script_pubkey: sync_core::bitcoin::ScriptBuf::from_bytes(script),
                });
            }
        }
    });
    let batch = LocalTweakIndex::new(with_p2wsh, IndexPolicy::Bip352)
        .fetch_range(2, 2)
        .unwrap();
    let Evidence::TweakIndex(entries) = &batch.blocks[0].evidence else {
        panic!("index evidence expected");
    };
    assert_eq!(
        entries[0]
            .outputs
            .iter()
            .map(|o| o.vout)
            .collect::<Vec<_>>(),
        vec![0, 2],
        "the P2WSH output at position 3 is not a taproot output"
    );
}

/// Detections of one transaction are reported by output index, whatever
/// order the matching (by `k`) finds them in.
#[test]
fn detections_of_one_transaction_are_ordered_by_output_index() {
    let receiver = fixture_gen::lab_receiver_keys().address(LabNetwork::Regtest);
    let txid = Txid::from_byte_array([0x43; 32]);
    // k = 0 sits at output 5, k = 1 at output 2.
    let entry = regtest_wallet::fabricated_index_entry(txid, receiver, &[(5, 500), (2, 200)]);
    let source = Tamper::new(index(IndexPolicy::Bip352), move |b: &mut ScanBatch| {
        if let Some(i) = b.blocks.iter().position(|blk| blk.block.height == 4) {
            indexed(b, i).push(entry.clone());
        }
    });
    let report = scan_range(&source, &scanner(), regtest(), 4, 4).unwrap();
    let found: Vec<(u32, u64)> = report
        .detected_received_outputs
        .iter()
        .filter(|d| d.txid == txid)
        .map(|d| (d.vout, d.value_sats))
        .collect();
    assert_eq!(found, vec![(2, 200), (5, 500)]);
}

/// The two index policies detect different outputs, so a store made with one
/// is not continued with the other.
#[test]
fn a_store_is_bound_to_the_index_policy() {
    let strict = index(IndexPolicy::Bip352);
    let lenient = index(IndexPolicy::IgnoreSegwitVersionRule);
    assert_ne!(strict.backend_id(), lenient.backend_id());

    let keys = fixture_gen::lab_receiver_keys();
    let scanner = scanner();
    let mut store = MemoryStore::new();
    store
        .initialize(&new_store_meta(
            &strict,
            &scanner,
            &keys.wallet_id(),
            1,
            "test",
        ))
        .unwrap();
    let half = SyncOptions {
        target_height: Some(5),
        ..SyncOptions::default()
    };
    sync(&strict, &scanner, &keys.wallet_id(), &mut store, &half).unwrap();
    let before = store.detections().unwrap();

    let result = sync(
        &lenient,
        &scanner,
        &keys.wallet_id(),
        &mut store,
        &SyncOptions::default(),
    );
    assert!(
        matches!(
            result,
            Err(sync_core::sync::SyncError::BindingMismatch {
                field: "backend",
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(store.detections().unwrap(), before);
}
