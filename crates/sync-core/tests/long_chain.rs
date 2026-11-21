//! Scale behaviour on a generated chain that is longer than one scan chunk
//! (design-spec case T06, and the chunk-border rules of `scan_range`).
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LongChain, LABEL_USED},
    test_source::Tamper,
};
use sync_core::{
    engine::{scan_range, ScanError, SCAN_CHUNK_BLOCKS},
    network::{ChainIdentity, LabNetwork},
    protocol::ReceiveScanner,
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
        ScanSource,
    },
    store::{MemoryStore, SyncStore},
    sync::{new_store_meta, sync, SyncOptions},
    verify::cross_check,
};

const BLOCKS: u32 = 250;

fn regtest() -> ChainIdentity {
    ChainIdentity::for_network(LabNetwork::Regtest)
}

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap()
}

/// (height, txid, vout, value, label)
type Expected = (u32, String, u32, u64, Option<u32>);

fn long_chain(shape: &LongChain) -> (FixtureSource, Vec<Expected>) {
    let fixture = fixture_gen::generate_long(shape);
    let expected = fixture
        .expected_detections
        .iter()
        .map(|e| (e.height, e.txid.to_string(), e.vout, e.value_sats, e.label))
        .collect();
    (FixtureSource::from_file(fixture).unwrap(), expected)
}

#[test]
fn the_generator_is_deterministic_and_shaped_as_documented() {
    let shape = LongChain::standard(30);
    let a = fixture_gen::generate_long(&shape);
    let b = fixture_gen::generate_long(&shape);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
    assert_eq!(a.blocks.len(), 30);
    assert_eq!(a.expected_detections.len(), 3);
    assert_eq!(
        a.expected_detections
            .iter()
            .map(|e| (e.height, e.label))
            .collect::<Vec<_>>(),
        vec![(10, None), (20, Some(LABEL_USED)), (30, None)]
    );
    // A shorter chain is a prefix of a longer one.
    let short = fixture_gen::generate_long(&LongChain::standard(12));
    assert_eq!(
        serde_json::to_string(&short.blocks).unwrap(),
        serde_json::to_string(&a.blocks[..12]).unwrap()
    );
}

/// A scan that spans several chunks finds exactly what the sender created,
/// and counts every transaction once.
#[test]
fn a_scan_across_chunk_borders_finds_every_payment_once() {
    // The chain must span three chunks.
    const { assert!(BLOCKS > 2 * SCAN_CHUNK_BLOCKS) };
    let (source, expected) = long_chain(&LongChain::standard(BLOCKS));
    assert_eq!(expected.len(), 25);

    let report = scan_range(&source, &scanner(), regtest(), 1, BLOCKS).unwrap();
    let found: Vec<_> = report
        .detected_received_outputs
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
        .collect();
    assert_eq!(found, expected);
    assert_eq!(report.blocks_scanned, u64::from(BLOCKS));
    assert_eq!(report.txs_seen, u64::from(BLOCKS) * 5);
    assert_eq!(
        report.txs_scanned + report.txs_skipped.values().sum::<u64>(),
        report.txs_seen
    );
    assert_eq!(
        (report.first_block.height, report.last_block.height),
        (1, BLOCKS)
    );

    // Ranges that start or end exactly on a chunk border.
    for (from, to) in [
        (1, SCAN_CHUNK_BLOCKS),
        (1, SCAN_CHUNK_BLOCKS + 1),
        (SCAN_CHUNK_BLOCKS, SCAN_CHUNK_BLOCKS + 1),
        (SCAN_CHUNK_BLOCKS + 1, 2 * SCAN_CHUNK_BLOCKS),
        (7, BLOCKS - 3),
    ] {
        let part = scan_range(&source, &scanner(), regtest(), from, to).unwrap();
        let want: Vec<_> = expected
            .iter()
            .filter(|e| (from..=to).contains(&e.0))
            .cloned()
            .collect();
        assert_eq!(
            part.detected_received_outputs.len(),
            want.len(),
            "{from}..={to}"
        );
        assert_eq!(part.blocks_scanned, u64::from(to - from + 1));
    }
}

/// The second chunk comes from another chain: every block of it is valid on
/// its own and links within the chunk, but not to the first chunk. The scan
/// must fail as a whole, not report the first chunk's payments as a result.
#[test]
fn a_chunk_that_does_not_link_to_the_previous_one_fails_the_scan() {
    let (main, _) = long_chain(&LongChain::standard(BLOCKS));
    let (other, _) = long_chain(&LongChain {
        payment_every: 7,
        ..LongChain::standard(BLOCKS)
    });
    assert_ne!(
        main.block_ref(SCAN_CHUNK_BLOCKS).unwrap().hash,
        other.block_ref(SCAN_CHUNK_BLOCKS).unwrap().hash
    );
    let spliced = Tamper::new(main, move |batch| {
        let (Some(first), Some(last)) = (batch.blocks.first(), batch.blocks.last()) else {
            return;
        };
        if first.block.height > SCAN_CHUNK_BLOCKS {
            *batch = other
                .fetch_range(first.block.height, last.block.height)
                .unwrap();
        }
    });

    match scan_range(&spliced, &scanner(), regtest(), 1, BLOCKS) {
        Err(ScanError::InconsistentBatch { reason, .. }) => assert!(
            reason.contains("block 101 does not link to block 100"),
            "{reason}"
        ),
        other => panic!("expected the scan to fail at the chunk border, got {other:?}"),
    }
    // Within the first chunk there is nothing to object to.
    assert!(scan_range(&spliced, &scanner(), regtest(), 1, SCAN_CHUNK_BLOCKS).is_ok());
    // The persistent sync refuses the same splice, keeping the first chunk.
    let mut store = MemoryStore::new();
    let wallet_id = fixture_gen::lab_receiver_keys().wallet_id();
    store
        .initialize(&new_store_meta(&spliced, &scanner(), &wallet_id, 1, "test"))
        .unwrap();
    let result = sync(
        &spliced,
        &scanner(),
        &wallet_id,
        &mut store,
        &SyncOptions::default(),
    );
    assert!(result.is_err(), "{result:?}");
    assert_eq!(
        store.checkpoint().unwrap().map(|c| c.height),
        Some(SCAN_CHUNK_BLOCKS)
    );
}

/// The persistent sync and the locally computed tweak index give the same
/// result as the stateless full-block scan, at this length too.
#[test]
fn sync_and_tweak_index_agree_with_the_full_scan_on_a_long_chain() {
    let (source, expected) = long_chain(&LongChain::standard(BLOCKS));
    let reference = scan_range(&source, &scanner(), regtest(), 1, BLOCKS).unwrap();

    for chunk in [1, 64, 100, 1_000] {
        let mut store = MemoryStore::new();
        let wallet_id = fixture_gen::lab_receiver_keys().wallet_id();
        store
            .initialize(&new_store_meta(&source, &scanner(), &wallet_id, 1, "test"))
            .unwrap();
        let report = sync(
            &source,
            &scanner(),
            &wallet_id,
            &mut store,
            &SyncOptions {
                chunk_size: chunk,
                ..SyncOptions::default()
            },
        )
        .unwrap();
        assert!(report.caught_up);
        assert_eq!(
            (report.txs_seen, report.txs_scanned, &report.txs_skipped),
            (
                reference.txs_seen,
                reference.txs_scanned,
                &reference.txs_skipped
            ),
            "chunk size {chunk}"
        );
        let mut stored = store.detections().unwrap();
        let mut want = reference.detected_received_outputs.clone();
        sync_core::store::sort_detections(&mut stored);
        sync_core::store::sort_detections(&mut want);
        assert_eq!(stored, want, "chunk size {chunk}");
    }

    let (again, _) = long_chain(&LongChain::standard(BLOCKS));
    let index = LocalTweakIndex::new(again, IndexPolicy::Bip352);
    let report = cross_check(&index, &source, &scanner(), regtest(), 1, BLOCKS).unwrap();
    assert!(report.identical, "{:?}", report.differences);
    assert_eq!(report.agreed, expected.len() as u64);
}
