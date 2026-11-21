//! Behavioural test suite for the persistent sync, generic over the
//! [`SyncStore`] implementation. Every store (in-memory, SQLite, ...) is run
//! through the same cases via [`sync_store_suite!`](crate::sync_store_suite).
//!
//! Covers design-spec cases T07 (interrupt and resume), T08 (restore from a
//! birthday vs. full rescan) and T09 (reorganization), plus the fail-closed
//! rules of `silent_sync::sync`.

use crate::{
    fixture_gen::{self, FORK_HEIGHT, LABEL_NOT_BACKED_UP, LABEL_USED},
    test_source::{Tamper, TestSource},
};
use silent_sync::{
    audit::{audit_store, AuditError, AuditOptions, StoreAuditReport},
    bitcoin::{hashes::Hash as _, OutPoint, Txid},
    engine::{scan_range, ScanError},
    models::{BlockRef, DetectedOutput, EvidenceKind},
    network::{ChainIdentity, Network},
    protocol::{ReceiveScanner, TxScanOutcome},
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
        Evidence, ScanBatch, ScanSource, SourceError,
    },
    store::{StoreError, StoreMeta, SyncStore, SCANNER_VERSION},
    sync::{
        new_store_meta, sync, sync_with_progress, StopReason, SyncError, SyncOptions, SyncReport,
    },
    verify::{Difference, Explanation, Side},
};

pub type StoreFactory<'a> = &'a dyn Fn() -> Box<dyn SyncStore>;

const MAIN_TIP: u32 = 8;
const FORK_TIP: u32 = 9;

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).expect("scanner")
}

fn wallet_id() -> String {
    fixture_gen::lab_receiver_keys().wallet_id()
}

fn init(store: &mut dyn SyncStore, source: &dyn ScanSource, birthday: u32) {
    let meta = new_store_meta(
        source,
        &scanner(),
        &wallet_id(),
        birthday,
        "test: fixture start",
    );
    store.initialize(&meta).expect("initialize");
}

fn options(chunk_size: u32) -> SyncOptions {
    SyncOptions::default().with_chunk_size(chunk_size)
}

fn run(source: &dyn ScanSource, store: &mut dyn SyncStore, opts: &SyncOptions) -> SyncReport {
    sync(source, &scanner(), &wallet_id(), store, opts).expect("sync")
}

/// What a stateless scan of `from..=to` on `chain` finds: the reference every
/// stored result is compared against.
fn reference(chain: &dyn ScanSource, from: u32, to: u32) -> Vec<DetectedOutput> {
    let regtest = ChainIdentity::for_network(Network::Regtest);
    let mut found = scan_range(chain, &scanner(), regtest, from, to)
        .expect("reference scan")
        .detected_received_outputs;
    silent_sync::store::sort_detections(&mut found);
    found
}

fn key(d: &DetectedOutput) -> (u32, Txid, u32) {
    (d.block.height, d.txid, d.vout)
}

/// Asserts the store is internally consistent and equals a fresh scan of
/// `chain` from `birthday` to its checkpoint.
fn assert_store_matches(store: &dyn SyncStore, chain: &dyn ScanSource, birthday: u32) {
    let checkpoint = store.checkpoint().expect("checkpoint");
    let stored = store.detections().expect("detections");
    match checkpoint {
        None => assert!(stored.is_empty(), "detections without any scanned block"),
        Some(cp) => {
            assert_eq!(
                cp,
                chain.block_ref(cp.height).expect("block"),
                "checkpoint not on chain"
            );
            assert_eq!(
                store.block_count().expect("count"),
                u64::from(cp.height - birthday + 1)
            );
            assert_eq!(stored, reference(chain, birthday, cp.height));
        }
    }
    let mut outpoints: Vec<OutPoint> = stored
        .iter()
        .map(|d| OutPoint::new(d.txid, d.vout))
        .collect();
    outpoints.sort();
    outpoints.dedup();
    assert_eq!(outpoints.len(), stored.len(), "an output is stored twice");
}

pub fn initial_sync_matches_a_stateless_scan(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    assert_eq!(store.checkpoint().expect("checkpoint"), None);

    let report = run(&source, store.as_mut(), &SyncOptions::default());
    assert!(report.caught_up);
    assert_eq!(report.resumed_from, None);
    assert_eq!(report.reorg, None);
    assert_eq!(report.blocks_scanned, 8);
    assert_eq!(report.txs_seen, 17);
    assert_eq!(report.txs_scanned, 6);
    assert_eq!(report.new_detections.len(), 7);
    assert_eq!(report.checkpoint, Some(source.tip().expect("tip")));
    assert_eq!(report.labels_scanned, vec![0, LABEL_USED]);
    assert_eq!(report.wallet_id, wallet_id());
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
    assert_eq!(store.detections().expect("detections").len(), 7);

    let meta = store.meta().expect("meta").expect("initialized");
    assert_eq!(meta.birthday, 1);
    assert_eq!(meta.birthday_basis, "test: fixture start");
    assert_eq!(meta.scanner_version, SCANNER_VERSION);
    assert_eq!(meta.backend_id, "fixture");
}

pub fn syncing_again_changes_nothing(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    let first = run(&source, store.as_mut(), &options(3));
    let before = store.detections().expect("detections");
    let fetches = source.fetch_calls.get();

    let again = run(&source, store.as_mut(), &options(3));
    assert!(again.caught_up);
    assert_eq!(again.resumed_from, first.checkpoint);
    assert_eq!((again.blocks_scanned, again.txs_seen), (0, 0));
    assert!(again.new_detections.is_empty());
    assert_eq!(store.detections().expect("detections"), before);
    assert_eq!(
        source.fetch_calls.get(),
        fetches,
        "nothing may be fetched twice"
    );
}

/// T07: stop half-way, resume, and at every other possible stopping point.
pub fn interrupted_sync_resumes_without_loss_or_duplicates(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let full = reference(source.main_chain(), 1, MAIN_TIP);

    // Stop at 50 %.
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    let half = sync(
        &source,
        &scanner(),
        &wallet_id(),
        store.as_mut(),
        &SyncOptions::default()
            .with_chunk_size(4)
            .with_max_blocks(Some(4))
            .with_target_height(None),
    )
    .expect("first half");
    assert!(!half.caught_up);
    assert_eq!(half.checkpoint.map(|c| c.height), Some(4));
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
    let rest = run(&source, store.as_mut(), &SyncOptions::default());
    assert!(rest.caught_up);
    assert_eq!(rest.resumed_from.map(|c| c.height), Some(4));
    assert_eq!(rest.blocks_scanned, 4);
    assert_eq!(store.detections().expect("detections"), full);
    let mut both: Vec<_> = half
        .new_detections
        .iter()
        .chain(&rest.new_detections)
        .map(key)
        .collect();
    both.sort();
    assert_eq!(
        both,
        full.iter().map(key).collect::<Vec<_>>(),
        "reports overlap or miss"
    );

    // Every chunk size, stopping after every chunk.
    for chunk in 1..=MAIN_TIP {
        let mut store = new_store();
        init(store.as_mut(), &source, 1);
        let mut calls = 0;
        loop {
            let report = sync(
                &source,
                &scanner(),
                &wallet_id(),
                store.as_mut(),
                &SyncOptions::default()
                    .with_chunk_size(chunk)
                    .with_max_blocks(Some(1))
                    .with_target_height(None),
            )
            .expect("step");
            assert_store_matches(store.as_ref(), source.main_chain(), 1);
            calls += 1;
            if report.caught_up {
                break;
            }
            assert!(calls < 20, "sync does not make progress");
        }
        assert_eq!(calls, MAIN_TIP.div_ceil(chunk), "chunk size {chunk}");
        assert_eq!(
            store.detections().expect("detections"),
            full,
            "chunk size {chunk}"
        );
    }
}

/// An outage in the middle: the error is reported, what was committed stays
/// consistent, and a later sync completes the job.
pub fn source_outage_leaves_a_resumable_store(new_store: StoreFactory<'_>) {
    for healthy_fetches in 0..4 {
        let source = TestSource::new();
        let mut store = new_store();
        init(store.as_mut(), &source, 1);
        source.fetches_left.set(Some(healthy_fetches));
        let result = sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &options(2),
        );
        let outage = |e: &SyncError| {
            matches!(
                e,
                SyncError::Scan(ScanError::Source(SourceError::Unavailable(_)))
            )
        };
        let committed = healthy_fetches * 2;
        match result {
            // Nothing was committed: the plain error.
            Err(ref e) if healthy_fetches == 0 => assert!(outage(e), "{e:?}"),
            // Chunks were committed before the outage: they are reported.
            Err(SyncError::Interrupted { cause, partial }) => {
                assert!(healthy_fetches > 0 && outage(&cause), "{cause:?}");
                assert_eq!(partial.stopped_because, StopReason::Error);
                assert!(!partial.caught_up);
                assert_eq!(partial.blocks_scanned, u64::from(committed));
                assert_eq!(partial.checkpoint.map(|c| c.height), Some(committed));
                assert_eq!(partial.reorg, None);
                assert_eq!(
                    partial.new_detections,
                    reference(source.main_chain(), 1, committed),
                    "the partial report lists exactly what was committed"
                );
            }
            other => panic!("expected an outage after {healthy_fetches} fetches, got {other:?}"),
        }
        assert_eq!(
            store.checkpoint().expect("checkpoint").map(|c| c.height),
            (healthy_fetches > 0).then_some(committed)
        );
        assert_store_matches(store.as_ref(), source.main_chain(), 1);

        source.fetches_left.set(None);
        let report = run(&source, store.as_mut(), &options(2));
        assert!(report.caught_up);
        assert_eq!(
            report.blocks_scanned,
            u64::from(MAIN_TIP - healthy_fetches * 2)
        );
        assert_eq!(
            store.detections().expect("detections"),
            reference(source.main_chain(), 1, MAIN_TIP)
        );
    }
}

/// A reorg rollback changes the store before anything is fetched. If the call
/// then fails, the rollback (and the detections it removed) must still be
/// reported: nothing is ever removed silently.
pub fn a_failure_after_a_reorg_rollback_still_reports_the_reorg(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(&source, store.as_mut(), &SyncOptions::default());
    let old_tip = store.checkpoint().expect("checkpoint").expect("synced");
    let on_main = store.detections().expect("detections");
    let removed: Vec<_> = on_main
        .iter()
        .filter(|d| d.block.height > FORK_HEIGHT)
        .cloned()
        .collect();
    assert_eq!(removed.len(), 2);

    // The chain reorganizes, and the source fails `healthy` fetches later.
    for healthy in 0..2u32 {
        let mut store = new_store();
        init(store.as_mut(), &source, 1);
        source.on_fork.set(false);
        source.fetches_left.set(None);
        run(&source, store.as_mut(), &SyncOptions::default());

        source.on_fork.set(true);
        source.fetches_left.set(Some(healthy));
        let result = sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &options(2),
        );
        let Err(SyncError::Interrupted { cause, partial }) = result else {
            panic!("expected an interrupted sync, got {result:?}");
        };
        assert!(
            matches!(
                *cause,
                SyncError::Scan(ScanError::Source(SourceError::Unavailable(_)))
            ),
            "{cause:?}"
        );
        let reorg = partial.reorg.clone().expect("the rollback is reported");
        assert_eq!(reorg.abandoned_checkpoint, old_tip);
        assert_eq!(reorg.fork_height, FORK_HEIGHT);
        assert_eq!(reorg.detections_removed, removed);
        assert_eq!(partial.blocks_scanned, u64::from(healthy * 2));
        assert_eq!(partial.stopped_because, StopReason::Error);
        assert_eq!(
            partial.checkpoint.map(|c| c.height),
            Some(FORK_HEIGHT + healthy * 2)
        );
        // The store is what the report says: the new chain up to there.
        assert_store_matches(store.as_ref(), source.fork_chain(), 1);
        assert_eq!(
            store.checkpoint().expect("c").map(|c| c.height),
            Some(FORK_HEIGHT + healthy * 2)
        );

        // The next call has no reorg left to report and finishes the job.
        source.fetches_left.set(None);
        let report = run(&source, store.as_mut(), &options(2));
        assert_eq!(report.reorg, None);
        assert!(report.caught_up);
        assert_store_matches(store.as_ref(), source.fork_chain(), 1);
    }
}

/// Every way a call can end without being caught up is named.
pub fn the_reason_a_sync_stopped_is_reported(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 3);

    // The source has not reached the birthday yet.
    source.visible_tip.set(Some(2));
    let report = run(&source, store.as_mut(), &SyncOptions::default());
    assert_eq!(report.stopped_because, StopReason::BirthdayAboveTip);
    assert!(!report.caught_up);
    assert_eq!((report.blocks_scanned, report.checkpoint), (0, None));
    source.visible_tip.set(None);

    let report = run(
        &source,
        store.as_mut(),
        &SyncOptions::default().with_target_height(Some(4)),
    );
    assert_eq!(report.stopped_because, StopReason::TargetReached);
    assert!(!report.caught_up);

    let report = run(
        &source,
        store.as_mut(),
        &SyncOptions::default()
            .with_chunk_size(1)
            .with_max_blocks(Some(1))
            .with_target_height(None),
    );
    assert_eq!(report.stopped_because, StopReason::MaxBlocksReached);
    assert_eq!(report.checkpoint.map(|c| c.height), Some(5));

    // A target equal to the tip is "caught up", not "target reached".
    let report = run(
        &source,
        store.as_mut(),
        &SyncOptions::default().with_target_height(Some(MAIN_TIP)),
    );
    assert_eq!(report.stopped_because, StopReason::CaughtUp);
    assert!(report.caught_up);
    // --max-blocks that is exactly what was left is also "caught up".
    let mut exact = new_store();
    init(exact.as_mut(), &source, 1);
    let report = run(
        &source,
        exact.as_mut(),
        &SyncOptions::default()
            .with_chunk_size(4)
            .with_max_blocks(Some(u64::from(MAIN_TIP)))
            .with_target_height(None),
    );
    assert_eq!(report.stopped_because, StopReason::CaughtUp);

    // The tip moved to another block of the same height while syncing: the
    // call reaches that height on blocks that are not the sampled tip.
    let mut raced = new_store();
    init(raced.as_mut(), &source, FORK_HEIGHT + 1);
    source.visible_tip.set(Some(MAIN_TIP));
    source.fetch_from_fork.set(Some(true));
    let report = run(&source, raced.as_mut(), &SyncOptions::default());
    assert_eq!(report.stopped_because, StopReason::TipChanged);
    assert!(!report.caught_up);
    assert_eq!(report.checkpoint.map(|c| c.height), Some(MAIN_TIP));
    assert_ne!(report.checkpoint, Some(report.source_tip));
    source.fetch_from_fork.set(None);
    source.visible_tip.set(None);
}

/// The progress callback sees every commit, in order, and the counters of the
/// chunks add up to those of one stateless scan.
pub fn progress_and_counters_cover_every_chunk(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let regtest = ChainIdentity::for_network(Network::Regtest);
    let whole = scan_range(source.main_chain(), &scanner(), regtest, 1, MAIN_TIP).expect("scan");
    assert!(
        whole.txs_skipped.len() >= 2 && whole.txs_skipped.values().sum::<u64>() > 2,
        "the fixture must exercise several skip reasons: {:?}",
        whole.txs_skipped
    );

    for chunk in [1, 3, 8] {
        let mut store = new_store();
        init(store.as_mut(), &source, 1);
        let mut commits = Vec::new();
        let report = sync_with_progress(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &options(chunk),
            &mut |checkpoint: BlockRef| commits.push(checkpoint.height),
        )
        .expect("sync");
        let expected: Vec<u32> = (1..=MAIN_TIP)
            .filter(|h| h % chunk == 0 || *h == MAIN_TIP)
            .collect();
        assert_eq!(commits, expected, "chunk size {chunk}");
        assert_eq!(report.txs_skipped, whole.txs_skipped, "chunk size {chunk}");
        assert_eq!(
            (report.txs_seen, report.txs_scanned, report.blocks_scanned),
            (whole.txs_seen, whole.txs_scanned, whole.blocks_scanned)
        );
    }
}

/// Stores return detections in one canonical order — by height, then by
/// `Txid`'s own ordering, then by output index — whatever order they were
/// appended in. The order of a txid's hex text is the reverse-byte order and
/// must not leak through.
pub fn detections_come_back_in_the_canonical_order(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let chain = source.main_chain();
    let block = |h: u32| chain.block_ref(h).expect("block");
    let template = reference(chain, 1, MAIN_TIP)
        .into_iter()
        .next()
        .expect("a detection");

    // `low` sorts first as a `Txid` but last as hex text.
    let mut bytes = [0u8; 32];
    bytes[0] = 0x01;
    bytes[31] = 0xff;
    let low = Txid::from_byte_array(bytes);
    bytes[0] = 0x02;
    bytes[31] = 0x00;
    let high = Txid::from_byte_array(bytes);
    assert!(low < high && low.to_string() > high.to_string());

    let made = |height: u32, txid: Txid, vout: u32| DetectedOutput {
        block: block(height),
        txid,
        vout,
        ..template.clone()
    };
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    store
        .append(
            &[block(1), block(2)],
            &[
                made(2, high, 0),
                made(2, low, 10),
                made(1, high, 1),
                made(2, low, 9),
                made(1, low, 0),
            ],
        )
        .expect("append");
    let canonical = vec![
        made(1, low, 0),
        made(1, high, 1),
        made(2, low, 9),
        made(2, low, 10),
        made(2, high, 0),
    ];
    assert_eq!(store.detections().expect("detections"), canonical);
    assert_eq!(store.rollback_to(1).expect("rollback"), canonical[2..]);
    assert_eq!(store.rollback_to(0).expect("rollback"), canonical[..2]);
}

/// T08: a store rebuilt from a birthday finds exactly what a full rescan finds
/// at or above that height — and nothing below it.
pub fn birthday_bounds_what_a_restored_wallet_finds(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let full = reference(source.main_chain(), 1, MAIN_TIP);
    assert_eq!(full.len(), 7);

    for birthday in 1..=MAIN_TIP {
        let mut store = new_store();
        init(store.as_mut(), &source, birthday);
        let report = run(&source, store.as_mut(), &options(3));
        assert!(report.caught_up);
        assert_eq!(report.birthday, birthday);
        assert_eq!(report.blocks_scanned, u64::from(MAIN_TIP - birthday + 1));
        let restored = store.detections().expect("detections");
        let expected: Vec<_> = full
            .iter()
            .filter(|d| d.block.height >= birthday)
            .cloned()
            .collect();
        assert_eq!(restored, expected, "birthday {birthday}");
        assert_eq!(
            store.block_at(birthday.saturating_sub(1)).expect("block"),
            None
        );
    }
    // Concretely: a birthday one block too late loses the first payment.
    let mut late = new_store();
    init(late.as_mut(), &source, 3);
    run(&source, late.as_mut(), &SyncOptions::default());
    let found = late.detections().expect("detections");
    assert_eq!(found.len(), 6);
    assert!(found.iter().all(|d| d.block.height >= 3));

    // Deleting the store and restoring with the same birthday is deterministic.
    let mut again = new_store();
    init(again.as_mut(), &source, 3);
    run(&source, again.as_mut(), &options(1));
    assert_eq!(again.detections().expect("detections"), found);
}

/// T09: the chain reorganizes below the checkpoint.
pub fn reorg_rolls_back_and_rescans(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(&source, store.as_mut(), &SyncOptions::default());
    let old_tip = store.checkpoint().expect("checkpoint").expect("synced");
    let on_main = store.detections().expect("detections");

    source.on_fork.set(true);
    let report = run(&source, store.as_mut(), &options(2));
    assert!(report.caught_up);
    assert_eq!(report.resumed_from, Some(old_tip));
    let reorg = report.reorg.clone().expect("a reorg must be reported");
    assert_eq!(reorg.abandoned_checkpoint, old_tip);
    assert_eq!(reorg.fork_height, FORK_HEIGHT);
    assert_eq!(reorg.blocks_rolled_back, u64::from(MAIN_TIP - FORK_HEIGHT));
    // Removed: the change-only payment (height 7) and the 1 sat payment (8).
    let removed: Vec<_> = on_main
        .iter()
        .filter(|d| d.block.height > FORK_HEIGHT)
        .cloned()
        .collect();
    assert_eq!(removed.len(), 2);
    assert_eq!(reorg.detections_removed, removed);
    assert_eq!(report.blocks_scanned, u64::from(FORK_TIP - FORK_HEIGHT));

    // Added: the fork-only payment (6') and the 1 sat payment again (8').
    let added: Vec<(u32, u64)> = report
        .new_detections
        .iter()
        .map(|d| (d.block.height, d.value_sats))
        .collect();
    assert_eq!(added, vec![(6, 33_000), (8, 1)]);
    // Same transaction, different block.
    let one_sat_before = removed.iter().find(|d| d.value_sats == 1).expect("1 sat");
    let one_sat_after = report
        .new_detections
        .iter()
        .find(|d| d.value_sats == 1)
        .expect("1 sat");
    assert_eq!(
        (one_sat_before.txid, one_sat_before.vout),
        (one_sat_after.txid, one_sat_after.vout)
    );
    assert_ne!(one_sat_before.block.hash, one_sat_after.block.hash);

    // The result is exactly a fresh sync of the new chain.
    assert_store_matches(store.as_ref(), source.fork_chain(), 1);
    let mut fresh = new_store();
    init(fresh.as_mut(), &source, 1);
    run(&source, fresh.as_mut(), &SyncOptions::default());
    assert_eq!(
        store.detections().expect("d"),
        fresh.detections().expect("d")
    );
    assert_eq!(
        store.checkpoint().expect("c"),
        fresh.checkpoint().expect("c")
    );
    assert_eq!(store.detections().expect("d").len(), 7);

    // And back again: the other branch is shorter but is a different chain at
    // its tip, so this is a reorg, not a lagging node.
    source.on_fork.set(false);
    let back = run(&source, store.as_mut(), &SyncOptions::default());
    assert_eq!(back.reorg.expect("reorg").fork_height, FORK_HEIGHT);
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
    assert_eq!(store.detections().expect("d"), on_main);
}

pub fn reorg_reaching_below_the_birthday_rescans_from_the_birthday(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let birthday = FORK_HEIGHT + 1;
    let mut store = new_store();
    init(store.as_mut(), &source, birthday);
    run(&source, store.as_mut(), &SyncOptions::default());
    assert_eq!(store.detections().expect("d").len(), 2);

    source.on_fork.set(true);
    let report = run(&source, store.as_mut(), &SyncOptions::default());
    let reorg = report.reorg.expect("reorg");
    assert_eq!(reorg.fork_height, birthday - 1, "no stored block is shared");
    assert_eq!(reorg.blocks_rolled_back, u64::from(MAIN_TIP - birthday + 1));
    assert_eq!(reorg.detections_removed.len(), 2);
    assert_store_matches(store.as_ref(), source.fork_chain(), birthday);
    assert_eq!(store.detections().expect("d").len(), 2);
}

/// A source may only make us discard as many scanned blocks as the caller
/// allows. A deeper "reorganization" is refused before anything is changed,
/// and without walking the store further down than the limit.
pub fn reorgs_deeper_than_the_limit_are_refused(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let depth = MAIN_TIP - FORK_HEIGHT; // 3 blocks of the main chain are not on the fork
    let synced_on_main = || {
        let mut store = new_store();
        source.on_fork.set(false);
        init(store.as_mut(), &source, 1);
        run(&source, store.as_mut(), &SyncOptions::default());
        source.on_fork.set(true);
        store
    };
    let with_limit = |limit: Option<u32>| SyncOptions::default().with_max_reorg_depth(limit);
    assert_eq!(
        SyncOptions::default().max_reorg_depth,
        Some(silent_sync::sync::DEFAULT_MAX_REORG_DEPTH)
    );

    for limit in [0, 1, depth - 1] {
        let mut store = synced_on_main();
        let before = (
            store.checkpoint().expect("c"),
            store.detections().expect("d"),
            store.block_count().expect("n"),
        );
        let result = sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &with_limit(Some(limit)),
        );
        assert_eq!(
            result,
            Err(SyncError::ReorgTooDeep {
                backend_id: "fixture".to_string(),
                checkpoint: MAIN_TIP,
                // The search stops one block past the limit.
                at_least: limit + 1,
                limit,
            }),
            "limit {limit}"
        );
        assert_eq!(
            (
                store.checkpoint().expect("c"),
                store.detections().expect("d"),
                store.block_count().expect("n"),
            ),
            before,
            "limit {limit}: a refused reorg must not change the store"
        );
    }

    // Exactly at the limit, above it, and without a limit: followed.
    for limit in [Some(depth), Some(depth + 1), None] {
        let mut store = synced_on_main();
        let report = run(&source, store.as_mut(), &with_limit(limit));
        let reorg = report.reorg.expect("reorg");
        assert_eq!(
            (reorg.fork_height, reorg.blocks_rolled_back),
            (FORK_HEIGHT, u64::from(depth)),
            "limit {limit:?}"
        );
        assert_store_matches(store.as_ref(), source.fork_chain(), 1);
    }

    // A fork below the birthday discards the whole store: that counts too.
    let birthday = FORK_HEIGHT + 1;
    let mut store = new_store();
    source.on_fork.set(false);
    init(store.as_mut(), &source, birthday);
    run(&source, store.as_mut(), &SyncOptions::default());
    source.on_fork.set(true);
    let stored = MAIN_TIP - birthday + 1;
    assert!(matches!(
        sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &with_limit(Some(stored - 1))
        ),
        Err(SyncError::ReorgTooDeep { .. })
    ));
    assert_eq!(store.block_count().expect("n"), u64::from(stored));
    let report = run(&source, store.as_mut(), &with_limit(Some(stored)));
    assert_eq!(
        report.reorg.expect("reorg").blocks_rolled_back,
        u64::from(stored)
    );
    source.on_fork.set(false);
}

/// A source that is merely behind must not make us forget scanned blocks.
pub fn a_source_behind_the_checkpoint_is_refused(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 2);
    run(&source, store.as_mut(), &SyncOptions::default());
    let before = (
        store.checkpoint().expect("c"),
        store.detections().expect("d"),
    );

    for visible in [1, 2, 5, MAIN_TIP - 1] {
        source.visible_tip.set(Some(visible));
        let result = sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &SyncOptions::default(),
        );
        assert_eq!(
            result,
            Err(SyncError::SourceBehindCheckpoint {
                backend_id: "fixture".to_string(),
                source_tip: visible,
                checkpoint: MAIN_TIP,
            }),
            "visible tip {visible}"
        );
        assert_eq!(
            (
                store.checkpoint().expect("c"),
                store.detections().expect("d")
            ),
            before
        );
    }
    source.visible_tip.set(None);
    assert!(run(&source, store.as_mut(), &SyncOptions::default()).caught_up);
}

pub fn target_and_chunk_limits_are_respected(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    let with_target = |target: u32| SyncOptions::default().with_target_height(Some(target));

    let report = run(&source, store.as_mut(), &with_target(4));
    assert!(!report.caught_up);
    assert_eq!(report.checkpoint.map(|c| c.height), Some(4));
    assert_store_matches(store.as_ref(), source.main_chain(), 1);

    // A target below the checkpoint neither scans nor rolls back.
    let report = run(&source, store.as_mut(), &with_target(2));
    assert_eq!(
        (report.blocks_scanned, report.checkpoint.map(|c| c.height)),
        (0, Some(4))
    );

    assert_eq!(
        sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &with_target(MAIN_TIP + 1)
        ),
        Err(SyncError::TargetAboveTip {
            target: MAIN_TIP + 1,
            tip: MAIN_TIP
        })
    );
    assert_eq!(
        sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &options(0)
        ),
        Err(SyncError::InvalidChunkSize)
    );
    assert_eq!(store.checkpoint().expect("c").map(|c| c.height), Some(4));

    // A chunk larger than what is left, and a huge chunk size.
    let report = run(&source, store.as_mut(), &options(u32::MAX));
    assert!(report.caught_up);
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
}

/// The chain changes between "where is the tip" and "give me the blocks".
pub fn blocks_that_do_not_extend_the_checkpoint_are_refused(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(
        &source,
        store.as_mut(),
        &SyncOptions::default().with_target_height(Some(6)),
    );
    let before = (
        store.checkpoint().expect("c"),
        store.detections().expect("d"),
    );

    // tip() / block_ref() still describe the main chain, fetch_range() serves the fork.
    source.fetch_from_fork.set(Some(true));
    assert_eq!(
        sync(
            &source,
            &scanner(),
            &wallet_id(),
            store.as_mut(),
            &SyncOptions::default()
        ),
        Err(SyncError::ChainMovedDuringSync { height: 7 })
    );
    assert_eq!(
        (
            store.checkpoint().expect("c"),
            store.detections().expect("d")
        ),
        before
    );

    source.fetch_from_fork.set(None);
    assert!(run(&source, store.as_mut(), &SyncOptions::default()).caught_up);
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
}

pub fn a_store_only_syncs_what_it_is_bound_to(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mismatch = |result: Result<SyncReport, SyncError>, expected_field: &str| match result {
        Err(SyncError::BindingMismatch { field, .. }) => assert_eq!(field, expected_field),
        other => panic!("expected a {expected_field} mismatch, got {other:?}"),
    };

    // Not initialized.
    let mut empty = new_store();
    assert_eq!(
        sync(
            &source,
            &scanner(),
            &wallet_id(),
            empty.as_mut(),
            &SyncOptions::default()
        ),
        Err(SyncError::Store(StoreError::NotInitialized))
    );

    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    let meta = store.meta().expect("meta").expect("initialized");
    assert_eq!(store.initialize(&meta), Err(StoreError::AlreadyInitialized));
    run(&source, store.as_mut(), &options(3));
    let before = store.detections().expect("d");
    let opts = SyncOptions::default();

    // Other labels: earlier blocks were not scanned for them.
    let more_labels = ReceiveScanner::new(
        &fixture_gen::lab_receiver_keys(),
        &[LABEL_USED, LABEL_NOT_BACKED_UP],
    )
    .expect("scanner");
    mismatch(
        sync(&source, &more_labels, &wallet_id(), store.as_mut(), &opts),
        "label set",
    );
    let fewer_labels =
        ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[]).expect("scanner");
    mismatch(
        sync(&source, &fewer_labels, &wallet_id(), store.as_mut(), &opts),
        "label set",
    );

    // Another wallet.
    let other_keys = fixture_gen::other_receiver_keys();
    let other = ReceiveScanner::new(&other_keys, &[LABEL_USED]).expect("scanner");
    assert_ne!(other_keys.wallet_id(), wallet_id());
    mismatch(
        sync(
            &source,
            &other,
            &other_keys.wallet_id(),
            store.as_mut(),
            &opts,
        ),
        "wallet",
    );

    // Another backend.
    let mut renamed = TestSource::new();
    renamed.backend_id = "something_else".to_string();
    mismatch(
        sync(&renamed, &scanner(), &wallet_id(), store.as_mut(), &opts),
        "backend",
    );

    // Another chain.
    source
        .chain_override
        .set(Some(ChainIdentity::for_network(Network::Signet)));
    mismatch(
        sync(&source, &scanner(), &wallet_id(), store.as_mut(), &opts),
        "chain",
    );
    source.chain_override.set(None);

    assert_eq!(
        store.detections().expect("d"),
        before,
        "a refused sync must not change the store"
    );

    // A store written by another scanner version.
    let mut old = new_store();
    let mut old_meta = meta.clone();
    old_meta.scanner_version = "sync-core/0.0.0".to_string();
    old.initialize(&old_meta).expect("initialize");
    mismatch(
        sync(&source, &scanner(), &wallet_id(), old.as_mut(), &opts),
        "scanner version",
    );
}

/// `sync` re-checks every batch itself: a source that returns fewer blocks
/// than requested must not be mistaken for progress.
pub fn short_batches_are_refused_by_sync(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(
        &source,
        store.as_mut(),
        &SyncOptions::default().with_target_height(Some(2)),
    );

    source.drop_last_block.set(true);
    let result = sync(
        &source,
        &scanner(),
        &wallet_id(),
        store.as_mut(),
        &options(3),
    );
    assert!(
        matches!(
            result,
            Err(SyncError::Scan(ScanError::InconsistentBatch { .. }))
        ),
        "{result:?}"
    );
    assert_eq!(store.checkpoint().expect("c").map(|c| c.height), Some(2));

    source.drop_last_block.set(false);
    assert!(run(&source, store.as_mut(), &options(3)).caught_up);
    assert_store_matches(store.as_ref(), source.main_chain(), 1);
}

/// The invariants every store enforces on its own, whoever calls it.
pub fn stores_reject_inconsistent_appends(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let chain = source.main_chain();
    let block = |h: u32| chain.block_ref(h).expect("block");
    let full = reference(chain, 1, MAIN_TIP);
    let at = |h: u32| -> Vec<DetectedOutput> {
        full.iter()
            .filter(|d| d.block.height == h)
            .cloned()
            .collect()
    };
    let invariant = |result: Result<(), StoreError>, needle: &str| match result {
        Err(StoreError::Invariant(msg)) => {
            assert!(msg.contains(needle), "`{msg}` should mention `{needle}`")
        }
        other => panic!("expected an invariant violation ({needle}), got {other:?}"),
    };

    let mut store = new_store();
    assert_eq!(
        store.append(&[block(1)], &[]),
        Err(StoreError::NotInitialized)
    );
    assert_eq!(store.rollback_to(0).err(), Some(StoreError::NotInitialized));
    assert_eq!(store.detections().err(), Some(StoreError::NotInitialized));
    let mut zero = new_store();
    let mut meta = new_store_meta(&source, &scanner(), &wallet_id(), 0, "x");
    invariant(zero.initialize(&meta), "birthday must be at least 1");
    meta.birthday = 2;
    store.initialize(&meta).expect("initialize");

    // The first block must be the birthday.
    invariant(store.append(&[block(1)], &[]), "must be the birthday 2");
    invariant(store.append(&[block(3)], &[]), "must be the birthday 2");
    invariant(store.append(&[], &[]), "zero blocks");
    store
        .append(&[block(2)], &at(2))
        .expect("append birthday block");

    // Must extend the checkpoint: no gap, no repeat, no foreign parent.
    invariant(
        store.append(&[block(4)], &[]),
        "does not extend the checkpoint",
    );
    invariant(
        store.append(&[block(2)], &[]),
        "does not extend the checkpoint",
    );
    let fork_block = source.fork_chain().block_ref(6).expect("fork block");
    let mut wrong_parent = block(3);
    wrong_parent.prev_hash = fork_block.hash;
    invariant(
        store.append(&[wrong_parent], &[]),
        "does not extend the checkpoint",
    );
    // Blocks within one call must be contiguous and linked.
    invariant(store.append(&[block(3), block(5)], &[]), "does not follow");
    let mut unlinked = block(4);
    unlinked.prev_hash = fork_block.hash;
    invariant(store.append(&[block(3), unlinked], &[]), "does not follow");

    // A block hash is recorded once, and no block is its own parent.
    let mut own_parent = block(3);
    own_parent.hash = own_parent.prev_hash;
    invariant(store.append(&[own_parent], &[]), "its own parent");
    let mut stored_again = block(4);
    stored_again.hash = block(2).hash;
    invariant(
        store.append(&[block(3), stored_again], &[]),
        "is already recorded",
    );
    let mut repeated = block(5);
    repeated.hash = block(3).hash;
    invariant(
        store.append(&[block(3), block(4), repeated], &[]),
        "is already recorded",
    );

    // Detections must belong to the appended blocks, once.
    invariant(
        store.append(&[block(3)], &at(5)),
        "which is not being appended",
    );
    // ... by hash, not just by height.
    let mut other_block = at(3);
    other_block[0].block.hash = fork_block.hash;
    invariant(
        store.append(&[block(3)], &other_block),
        "which is not being appended",
    );
    // A value no store can represent, and a detection from another backend.
    let mut huge = at(3);
    huge[0].value_sats = u64::MAX;
    invariant(store.append(&[block(3)], &huge), "does not fit the store");
    let mut at_limit = at(3);
    at_limit[0].value_sats = silent_sync::store::MAX_STORABLE_SATS + 1;
    invariant(
        store.append(&[block(3)], &at_limit),
        "does not fit the store",
    );
    let mut other_backend = at(3);
    other_backend[0].provenance.backend_id = "something_else".to_string();
    invariant(
        store.append(&[block(3)], &other_backend),
        "detection from backend `something_else`",
    );
    let mut twice = at(3);
    twice.extend(at(3));
    invariant(store.append(&[block(3)], &twice), "recorded twice");
    let mut already = at(3);
    let mut relocated = at(2)[0].clone();
    relocated.block = block(3);
    already.push(relocated);
    invariant(store.append(&[block(3)], &already), "recorded twice");

    // A detection that claims another network.
    let mut foreign = at(3);
    foreign[0].network = Network::Signet;
    invariant(
        store.append(&[block(3)], &foreign),
        "detection on signet in a regtest store",
    );

    // Nothing of the rejected calls was kept.
    assert_eq!(store.checkpoint().expect("c"), Some(block(2)));
    assert_eq!(store.detections().expect("d"), at(2));
    assert_eq!(store.block_count().expect("n"), 1);

    // A valid multi-block append, then rollbacks.
    store
        .append(&[block(3), block(4), block(5)], &[at(3), at(5)].concat())
        .expect("append");
    assert_eq!(store.block_count().expect("n"), 4);
    assert_eq!(store.block_at(4).expect("b"), Some(block(4)));
    assert_eq!(store.block_at(6).expect("b"), None);
    let removed = store.rollback_to(4).expect("rollback");
    assert_eq!(removed, at(5));
    assert_eq!(store.checkpoint().expect("c"), Some(block(4)));
    assert_eq!(
        store.rollback_to(4).expect("rollback"),
        vec![],
        "idempotent"
    );
    assert_eq!(
        store.rollback_to(9).expect("rollback"),
        vec![],
        "above the checkpoint: no-op"
    );
    let removed = store.rollback_to(0).expect("rollback");
    assert_eq!(removed, [at(2), at(3)].concat());
    assert_eq!(store.checkpoint().expect("c"), None);
    assert_eq!(store.block_count().expect("n"), 0);
    // After a full rollback the birthday rule applies again.
    invariant(store.append(&[block(3)], &[]), "must be the birthday 2");
    // The largest representable value is accepted and read back unchanged.
    let mut largest = at(2);
    largest[0].value_sats = silent_sync::store::MAX_STORABLE_SATS;
    store.append(&[block(2)], &largest).expect("append");
    assert_eq!(store.detections().expect("d"), largest);
}

/// Reference helper for implementation-specific tests.
pub fn block_refs(from: u32, to: u32) -> Vec<BlockRef> {
    let source = TestSource::new();
    (from..=to)
        .map(|h| source.main_chain().block_ref(h).expect("block"))
        .collect()
}

// ---------------------------------------------------------------------------
// audit_store: what a store holds, compared with a reference scan
// ---------------------------------------------------------------------------

fn audit(
    reference: &dyn ScanSource,
    store: &dyn SyncStore,
    options: &AuditOptions,
) -> Result<StoreAuditReport, AuditError> {
    audit_store(reference, &scanner(), &wallet_id(), store, options)
}

/// A store holding the main chain's blocks with `edit` applied to what an
/// honest scan detects: the store of someone whose source lied, or whose
/// file was altered.
fn store_holding(
    new_store: StoreFactory<'_>,
    source: &TestSource,
    edit: &dyn Fn(&mut Vec<DetectedOutput>),
) -> Box<dyn SyncStore> {
    let mut store = new_store();
    init(store.as_mut(), source, 1);
    let blocks = source.main_chain().block_refs(1, MAIN_TIP).expect("refs");
    let mut detections = reference(source.main_chain(), 1, MAIN_TIP);
    edit(&mut detections);
    store.append(&blocks, &detections).expect("append");
    store
}

/// Read-only view of a store that misreports what it holds: stands for a
/// store whose own consistency rules were bypassed.
struct Misreporting<'a> {
    inner: &'a dyn SyncStore,
    detections_claim_another_block: bool,
    missing_block: Option<u32>,
}

impl SyncStore for Misreporting<'_> {
    fn meta(&self) -> Result<Option<StoreMeta>, StoreError> {
        self.inner.meta()
    }
    fn initialize(&mut self, _: &StoreMeta) -> Result<(), StoreError> {
        Err(StoreError::Backend("read-only view".to_string()))
    }
    fn checkpoint(&self) -> Result<Option<BlockRef>, StoreError> {
        self.inner.checkpoint()
    }
    fn block_at(&self, height: u32) -> Result<Option<BlockRef>, StoreError> {
        if self.missing_block == Some(height) {
            return Ok(None);
        }
        self.inner.block_at(height)
    }
    fn append(&mut self, _: &[BlockRef], _: &[DetectedOutput]) -> Result<(), StoreError> {
        Err(StoreError::Backend("read-only view".to_string()))
    }
    fn rollback_to(&mut self, _: u32) -> Result<Vec<DetectedOutput>, StoreError> {
        Err(StoreError::Backend("read-only view".to_string()))
    }
    fn detections(&self) -> Result<Vec<DetectedOutput>, StoreError> {
        let mut all = self.inner.detections()?;
        if self.detections_claim_another_block {
            for d in &mut all {
                d.block.hash = d.block.prev_hash;
            }
        }
        Ok(all)
    }
    fn block_count(&self) -> Result<u64, StoreError> {
        self.inner.block_count()
    }
}

/// A synced store equals a fresh scan through a reference — whichever
/// backend it was filled from, and whichever kind of reference is used.
pub fn audit_of_an_honest_store_finds_no_difference(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(&source, store.as_mut(), &options(3));
    let before = (
        store.checkpoint().expect("c"),
        store.detections().expect("d"),
        store.block_count().expect("n"),
    );

    let report = audit(&source, store.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(report.identical);
    assert_eq!(
        (
            report.first_height,
            report.last_height,
            report.blocks_compared
        ),
        (1, MAIN_TIP, u64::from(MAIN_TIP))
    );
    assert_eq!(
        (report.agreed, report.stored_only, report.reference_only),
        (7, 0, 0)
    );
    assert!(report.differences.is_empty() && report.block_hash_mismatches.is_empty());
    assert_eq!(report.unexplained, 0);
    assert_eq!(report.store_backend, "fixture");
    assert_eq!(report.reference_backend, "fixture");
    assert_eq!(
        report.reference_evidence,
        EvidenceKind::FullBlockWithPrevouts
    );
    assert_eq!(report.labels_scanned, vec![0, LABEL_USED]);
    assert_eq!(report.chain, ChainIdentity::for_network(Network::Regtest));

    // Part of the stored range.
    let part = AuditOptions::default()
        .with_first_height(Some(3))
        .with_last_height(Some(5));
    let report = audit(&source, store.as_ref(), &part).expect("audit");
    assert!(report.identical);
    assert_eq!(
        (
            report.first_height,
            report.last_height,
            report.blocks_compared
        ),
        (3, 5, 3)
    );
    assert_eq!(
        report.agreed,
        reference(source.main_chain(), 3, 5).len() as u64
    );
    assert!(report.agreed > 0 && report.agreed < 7);

    // Another backend as the reference: a tweak index over the same chain.
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture_gen::generate()).expect("fixture"),
        IndexPolicy::Bip352,
    );
    let report = audit(&index, store.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(report.identical);
    assert_eq!(report.agreed, 7);
    assert_ne!(report.reference_backend, report.store_backend);
    assert_eq!(report.reference_evidence, EvidenceKind::TweakIndex);

    // A store filled from an index, audited against full blocks: the case
    // the audit exists for.
    let mut from_index = new_store();
    init(from_index.as_mut(), &index, 1);
    run(&index, from_index.as_mut(), &SyncOptions::default());
    let report = audit(&source, from_index.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(report.identical);
    assert_eq!(report.agreed, 7);
    assert_eq!(report.store_backend, index.backend_id());
    assert_eq!(report.reference_backend, "fixture");

    // An audit only reads.
    assert_eq!(
        (
            store.checkpoint().expect("c"),
            store.detections().expect("d"),
            store.block_count().expect("n"),
        ),
        before
    );
}

/// Whatever the store lacks, has in addition, or holds differently is
/// reported output by output, with the cause the reference blocks allow.
pub fn audit_reports_what_a_store_lacks_adds_or_alters(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let honest = reference(source.main_chain(), 1, MAIN_TIP);
    assert_eq!(honest.len(), 7);
    let find = |report: &StoreAuditReport, txid: Txid, vout: u32| -> Difference {
        let found: Vec<&Difference> = report
            .differences
            .iter()
            .filter(|d| d.txid == txid && d.vout == vout)
            .collect();
        assert_eq!(found.len(), 1, "{txid}:{vout} in {:?}", report.differences);
        found[0].clone()
    };

    // A payment the store's source never reported.
    let lacking = store_holding(new_store, &source, &|d| {
        d.remove(2);
    });
    let report = audit(&source, lacking.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(!report.identical);
    assert_eq!(
        (report.agreed, report.stored_only, report.reference_only),
        (6, 0, 1)
    );
    assert_eq!(report.unexplained, 0);
    assert!(report.block_hash_mismatches.is_empty());
    let missing = &honest[2];
    let d = find(&report, missing.txid, missing.vout);
    assert_eq!(d.height, missing.block.height);
    assert_eq!(d.only_in, Some(Side::Reference));
    assert_eq!(d.candidate, None);
    assert_eq!(d.reference.as_ref(), Some(missing));
    assert_eq!(d.explanation, Explanation::StoreOmittedOutput);

    // The same payment hidden from an audit that does not cover its block.
    let elsewhere = AuditOptions::default().with_last_height(Some(missing.block.height - 1));
    assert!(
        audit(&source, lacking.as_ref(), &elsewhere)
            .expect("audit")
            .identical,
        "an audit says nothing about heights outside its range"
    );

    // An amount and a label that are not what the chain says.
    let altered = store_holding(new_store, &source, &|d| {
        d[0].value_sats += 1;
        d[4].label = Some(LABEL_NOT_BACKED_UP);
    });
    let report = audit(&source, altered.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(!report.identical);
    assert_eq!(
        (report.agreed, report.stored_only, report.reference_only),
        (5, 0, 0)
    );
    for i in [0, 4] {
        let d = find(&report, honest[i].txid, honest[i].vout);
        assert_eq!(d.only_in, None);
        assert_eq!(d.explanation, Explanation::DetailsDiffer);
        assert_eq!(d.reference.as_ref(), Some(&honest[i]));
        assert_ne!(d.candidate.as_ref(), Some(&honest[i]));
    }

    // Outputs the reference does not detect, one for each cause it can name.
    let full = source
        .main_chain()
        .fetch_range(1, MAIN_TIP)
        .expect("blocks");
    let not_eligible = full
        .blocks
        .iter()
        .find_map(|b| {
            let Evidence::FullBlock(txs) = &b.evidence else {
                return None;
            };
            txs.iter().find_map(
                |t| match scanner().scan_tx(&t.tx, &t.prevouts).expect("scan") {
                    TxScanOutcome::NotEligible(_) => Some((b.block, t.tx.compute_txid())),
                    _ => None,
                },
            )
        })
        .expect("the fixture has a transaction BIP352 says not to scan");
    let invented_txid = Txid::from_byte_array([7; 32]);
    let moved = honest[1].clone();
    let added = store_holding(new_store, &source, &|d| {
        // (1) a transaction that is not in the block at all;
        let mut invented = d[0].clone();
        invented.txid = invented_txid;
        // (2) a transaction BIP352 says must not be scanned;
        let mut ineligible = d[0].clone();
        ineligible.block = not_eligible.0;
        ineligible.txid = not_eligible.1;
        ineligible.vout = 0;
        // (3) a paying transaction, but an output that does not match.
        d[1].vout += 50;
        d.push(invented);
        d.push(ineligible);
    });
    let report = audit(&source, added.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(!report.identical);
    assert_eq!(
        (report.agreed, report.stored_only, report.reference_only),
        (6, 3, 1)
    );
    assert_eq!(report.unexplained, 0);
    let d = find(&report, invented_txid, honest[0].vout);
    assert_eq!(d.only_in, Some(Side::Candidate));
    assert_eq!(d.reference, None);
    assert_eq!(d.explanation, Explanation::ReferenceTransactionMissing);
    let d = find(&report, not_eligible.1, 0);
    assert_eq!(d.height, not_eligible.0.height);
    assert!(
        matches!(d.explanation, Explanation::ReferenceNotEligible { .. }),
        "{:?}",
        d.explanation
    );
    let d = find(&report, moved.txid, moved.vout + 50);
    assert_eq!(d.only_in, Some(Side::Candidate));
    assert_eq!(d.explanation, Explanation::ReferenceScannedNoMatch);
    let d = find(&report, moved.txid, moved.vout);
    assert_eq!(d.explanation, Explanation::StoreOmittedOutput);
    let heights: Vec<u32> = report.differences.iter().map(|d| d.height).collect();
    let mut ascending = heights.clone();
    ascending.sort_unstable();
    assert_eq!(heights, ascending, "differences are ordered by height");

    // Against a reference that cannot show transactions, a stored-only
    // output is reported, but not explained.
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture_gen::generate()).expect("fixture"),
        IndexPolicy::Bip352,
    );
    let report = audit(&index, added.as_ref(), &AuditOptions::default()).expect("audit");
    assert_eq!((report.stored_only, report.unexplained), (3, 3));
}

/// A store that is on another chain than the reference is told where; its
/// detections there are not reported as agreeing.
pub fn audit_names_the_blocks_of_another_chain(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let mut store = new_store();
    init(store.as_mut(), &source, 1);
    run(&source, store.as_mut(), &SyncOptions::default());
    source.on_fork.set(true);

    let report = audit(&source, store.as_ref(), &AuditOptions::default()).expect("audit");
    assert!(!report.identical);
    assert_eq!(
        report.block_hash_mismatches,
        (FORK_HEIGHT + 1..=MAIN_TIP).collect::<Vec<u32>>()
    );
    assert_eq!(report.blocks_compared, u64::from(MAIN_TIP));
    let below_fork = reference(source.main_chain(), 1, FORK_HEIGHT).len() as u64;
    assert_eq!(report.agreed, below_fork);
    assert!(!report.differences.is_empty());
    assert!(report
        .differences
        .iter()
        .all(|d| d.height > FORK_HEIGHT && d.explanation == Explanation::BlockHashDiffers));
    // Every output either side has above the fork is listed.
    let main_above = reference(source.main_chain(), FORK_HEIGHT + 1, MAIN_TIP);
    let fork_above = reference(source.fork_chain(), FORK_HEIGHT + 1, MAIN_TIP);
    let mut expected: Vec<(Txid, u32)> = main_above
        .iter()
        .chain(&fork_above)
        .map(|d| (d.txid, d.vout))
        .collect();
    expected.sort();
    expected.dedup();
    let mut listed: Vec<(Txid, u32)> = report
        .differences
        .iter()
        .map(|d| (d.txid, d.vout))
        .collect();
    listed.sort();
    assert_eq!(listed, expected);

    // Below the fork the two chains are the same chain.
    let shared = AuditOptions::default().with_last_height(Some(FORK_HEIGHT));
    assert!(
        audit(&source, store.as_ref(), &shared)
            .expect("audit")
            .identical
    );
}

/// An audit that cannot be carried out is an error, never "no difference".
pub fn audit_refuses_what_it_cannot_carry_out(new_store: StoreFactory<'_>) {
    let source = TestSource::new();
    let all = AuditOptions::default();

    // No binding, and nothing scanned yet.
    let mut store = new_store();
    assert_eq!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::Store(StoreError::NotInitialized))
    );
    init(store.as_mut(), &source, 3);
    assert_eq!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::NothingStored)
    );
    run(&source, store.as_mut(), &SyncOptions::default());
    assert!(
        audit(&source, store.as_ref(), &all)
            .expect("audit")
            .identical
    );

    // Ranges that are empty or reach outside what is stored (3..=8).
    for (first, last) in [
        (Some(2), None),
        (None, Some(MAIN_TIP + 1)),
        (Some(6), Some(5)),
        (Some(0), Some(0)),
    ] {
        let range = AuditOptions::default()
            .with_first_height(first)
            .with_last_height(last);
        assert_eq!(
            audit(&source, store.as_ref(), &range),
            Err(AuditError::RangeOutsideStore {
                first: first.unwrap_or(3),
                last: last.unwrap_or(MAIN_TIP),
                first_stored: 3,
                last_stored: MAIN_TIP,
            }),
            "{first:?}..={last:?}"
        );
    }
    for (first, last) in [(3, 3), (MAIN_TIP, MAIN_TIP), (3, MAIN_TIP)] {
        let range = AuditOptions::default()
            .with_first_height(Some(first))
            .with_last_height(Some(last));
        let report = audit(&source, store.as_ref(), &range).expect("audit");
        assert_eq!(report.blocks_compared, u64::from(last - first + 1));
    }

    // Another receiver, another label set, another chain.
    let other = fixture_gen::other_receiver_keys();
    let other_scanner = ReceiveScanner::new(&other, &[LABEL_USED]).expect("scanner");
    assert!(matches!(
        audit_store(
            &source,
            &other_scanner,
            &other.wallet_id(),
            store.as_ref(),
            &all
        ),
        Err(AuditError::BindingMismatch {
            field: "wallet",
            ..
        })
    ));
    let more_labels = ReceiveScanner::new(
        &fixture_gen::lab_receiver_keys(),
        &[LABEL_USED, LABEL_NOT_BACKED_UP],
    )
    .expect("scanner");
    assert!(matches!(
        audit_store(&source, &more_labels, &wallet_id(), store.as_ref(), &all),
        Err(AuditError::BindingMismatch {
            field: "label set",
            ..
        })
    ));
    source
        .chain_override
        .set(Some(ChainIdentity::for_network(Network::Signet)));
    assert!(matches!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::BindingMismatch { field: "chain", .. })
    ));
    source.chain_override.set(None);

    // A reference that cannot serve the range, or serves it inconsistently.
    source.visible_tip.set(Some(MAIN_TIP - 1));
    assert!(matches!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::Scan(ScanError::Source(
            SourceError::RangeUnavailable { .. }
        )))
    ));
    source.visible_tip.set(None);
    source.fetches_left.set(Some(0));
    assert!(matches!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::Scan(ScanError::Source(
            SourceError::Unavailable(_)
        )))
    ));
    source.fetches_left.set(None);
    source.drop_last_block.set(true);
    assert!(matches!(
        audit(&source, store.as_ref(), &all),
        Err(AuditError::Scan(ScanError::InconsistentBatch { .. }))
    ));
    source.drop_last_block.set(false);
    // The same transaction listed in two blocks of the range.
    let replaying = Tamper::new(TestSource::new(), |batch: &mut ScanBatch| {
        let Some(paying) = batch.blocks.iter().find_map(|b| match &b.evidence {
            Evidence::FullBlock(txs) => txs.get(1).cloned(),
            _ => None,
        }) else {
            return;
        };
        if let Some(Evidence::FullBlock(txs)) = batch.blocks.last_mut().map(|b| &mut b.evidence) {
            if !txs.contains(&paying) {
                txs.push(paying);
            }
        }
    });
    assert!(matches!(
        audit(&replaying, store.as_ref(), &all),
        Err(AuditError::Scan(ScanError::InconsistentBatch { .. }))
    ));

    // A store that misreports itself is an error, not a difference.
    let claims_other_blocks = Misreporting {
        inner: store.as_ref(),
        detections_claim_another_block: true,
        missing_block: None,
    };
    match audit(&source, &claims_other_blocks, &all) {
        Err(AuditError::Store(StoreError::Invariant(reason))) => {
            assert!(reason.contains("belongs to block"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
    let lacks_a_block = Misreporting {
        inner: store.as_ref(),
        detections_claim_another_block: false,
        missing_block: Some(5),
    };
    match audit(&source, &lacks_a_block, &all) {
        Err(AuditError::Store(StoreError::Invariant(reason))) => {
            assert!(reason.contains("no stored block at height 5"), "{reason}")
        }
        other => panic!("{other:?}"),
    }
}

/// Expands to one `#[test]` per suite case, each run against stores produced
/// by `$factory` (an expression of type `fn() -> Box<dyn SyncStore>`).
#[macro_export]
macro_rules! sync_store_suite {
    ($factory:expr) => {
        $crate::sync_store_suite!(@cases $factory;
            initial_sync_matches_a_stateless_scan,
            syncing_again_changes_nothing,
            interrupted_sync_resumes_without_loss_or_duplicates,
            source_outage_leaves_a_resumable_store,
            a_failure_after_a_reorg_rollback_still_reports_the_reorg,
            the_reason_a_sync_stopped_is_reported,
            progress_and_counters_cover_every_chunk,
            detections_come_back_in_the_canonical_order,
            birthday_bounds_what_a_restored_wallet_finds,
            reorg_rolls_back_and_rescans,
            reorg_reaching_below_the_birthday_rescans_from_the_birthday,
            reorgs_deeper_than_the_limit_are_refused,
            a_source_behind_the_checkpoint_is_refused,
            target_and_chunk_limits_are_respected,
            blocks_that_do_not_extend_the_checkpoint_are_refused,
            a_store_only_syncs_what_it_is_bound_to,
            short_batches_are_refused_by_sync,
            stores_reject_inconsistent_appends,
            audit_of_an_honest_store_finds_no_difference,
            audit_reports_what_a_store_lacks_adds_or_alters,
            audit_names_the_blocks_of_another_chain,
            audit_refuses_what_it_cannot_carry_out,
        );
    };
    (@cases $factory:expr; $($case:ident,)*) => {
        $(
            #[test]
            fn $case() {
                $crate::sync_suite::$case(&$factory);
            }
        )*
    };
}
