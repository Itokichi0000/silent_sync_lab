//! Behavioural test suite for the persistent sync, generic over the
//! [`SyncStore`] implementation. Every store (in-memory, SQLite, ...) is run
//! through the same cases via [`sync_store_suite!`](crate::sync_store_suite).
//!
//! Covers design-spec cases T07 (interrupt and resume), T08 (restore from a
//! birthday vs. full rescan) and T09 (reorganization), plus the fail-closed
//! rules of `sync_core::sync`.

use crate::{
    fixture_gen::{self, FORK_HEIGHT, LABEL_NOT_BACKED_UP, LABEL_USED},
    test_source::TestSource,
};
use sync_core::{
    bitcoin::{OutPoint, Txid},
    engine::{scan_range, ScanError},
    models::{BlockRef, DetectedOutput},
    network::{ChainIdentity, LabNetwork},
    protocol::ReceiveScanner,
    source::{ScanSource, SourceError},
    store::{StoreError, SyncStore, SCANNER_VERSION},
    sync::{new_store_meta, sync, SyncError, SyncOptions, SyncReport},
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
    SyncOptions {
        chunk_size,
        ..SyncOptions::default()
    }
}

fn run(source: &dyn ScanSource, store: &mut dyn SyncStore, opts: &SyncOptions) -> SyncReport {
    sync(source, &scanner(), &wallet_id(), store, opts).expect("sync")
}

/// What a stateless scan of `from..=to` on `chain` finds: the reference every
/// stored result is compared against.
fn reference(chain: &dyn ScanSource, from: u32, to: u32) -> Vec<DetectedOutput> {
    let regtest = ChainIdentity::for_network(LabNetwork::Regtest);
    let mut found = scan_range(chain, &scanner(), regtest, from, to)
        .expect("reference scan")
        .detected_received_outputs;
    sync_core::store::sort_detections(&mut found);
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
        &SyncOptions {
            chunk_size: 4,
            max_blocks: Some(4),
            target_height: None,
        },
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
                &SyncOptions {
                    chunk_size: chunk,
                    max_blocks: Some(1),
                    target_height: None,
                },
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
        assert!(
            matches!(
                result,
                Err(SyncError::Scan(ScanError::Source(
                    SourceError::Unavailable(_)
                )))
            ),
            "{result:?}"
        );
        assert_eq!(
            store.checkpoint().expect("checkpoint").map(|c| c.height),
            (healthy_fetches > 0).then_some(healthy_fetches * 2)
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
    let with_target = |target: u32| SyncOptions {
        target_height: Some(target),
        ..SyncOptions::default()
    };

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
        &SyncOptions {
            target_height: Some(6),
            ..SyncOptions::default()
        },
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
        .set(Some(ChainIdentity::for_network(LabNetwork::Signet)));
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
        &SyncOptions {
            target_height: Some(2),
            ..SyncOptions::default()
        },
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

    // Detections must belong to the appended blocks, once.
    invariant(
        store.append(&[block(3)], &at(5)),
        "which is not being appended",
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
    foreign[0].network = LabNetwork::Signet;
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
    store.append(&[block(2)], &at(2)).expect("append");
}

/// Reference helper for implementation-specific tests.
pub fn block_refs(from: u32, to: u32) -> Vec<BlockRef> {
    let source = TestSource::new();
    (from..=to)
        .map(|h| source.main_chain().block_ref(h).expect("block"))
        .collect()
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
            birthday_bounds_what_a_restored_wallet_finds,
            reorg_rolls_back_and_rescans,
            reorg_reaching_below_the_birthday_rescans_from_the_birthday,
            a_source_behind_the_checkpoint_is_refused,
            target_and_chunk_limits_are_respected,
            blocks_that_do_not_extend_the_checkpoint_are_refused,
            a_store_only_syncs_what_it_is_bound_to,
            short_batches_are_refused_by_sync,
            stores_reject_inconsistent_appends,
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
