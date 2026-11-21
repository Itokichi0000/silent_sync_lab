//! Phase 5 — fault injection (design-spec cases T10–T12). The matrix itself
//! lives in `lab_testkit::fault_matrix`; this file pins its verdicts, so that
//! "what is detected and what is not" cannot change unnoticed, and keeps
//! `docs/FAULT_INJECTION.md` in step with what the code actually does.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fault_matrix::{self, Alone, ReplayingOracle, Verdict},
    fixture_gen::{self, LABEL_USED},
};
use silent_sync::{
    protocol::ReceiveScanner,
    store::{MemoryStore, StoreError, SyncStore},
    sync::{new_store_meta, sync, SyncError, SyncOptions},
};

const DOC: &str = include_str!("../../../docs/FAULT_INJECTION.md");

#[test]
fn every_fault_has_the_documented_verdict() {
    use Verdict::*;
    let expected = [
        // Full blocks from a node: everything a block commits to is checked.
        ("F01", Refused),
        ("F02", Refused),
        ("F03", Refused),
        ("F04", Refused),
        // Nothing commits to spent outputs from before the range.
        ("F05", SilentAloneShownByCrossCheck),
        // A source that hides its newest blocks looks like a shorter chain.
        ("F06", SilentAloneShownByCrossCheck),
        ("F07", Refused),
        ("F08", Refused),
        ("F09", Refused),
        ("F10", Refused),
        ("F11", Refused),
        // A different chain that is consistent in itself is a valid chain as
        // far as one source can tell (proof of work is trivial on regtest).
        ("F12", SilentAloneShownByCrossCheck),
        // A tweak index: omissions, alterations and inventions are silent.
        ("I01", SilentAloneShownByCrossCheck),
        ("I02", SilentAloneShownByCrossCheck),
        ("I03", SilentAloneShownByCrossCheck),
        ("I04", SilentAloneShownByCrossCheck),
        ("I05", SilentAloneShownByCrossCheck),
        ("I06", SilentAloneShownByCrossCheck),
        ("I07", Refused),
        ("I08", Refused),
        ("I09", NoEffect),
        ("I10", NoEffect),
        ("I11", Refused),
        ("I12", Refused),
        ("I13", Refused),
        ("I14", Refused),
        ("I15", Refused),
        ("I16", Refused),
        // Claims no block can satisfy are refused, whoever they concern.
        ("I17", Refused),
        ("I18", Refused),
        // A lie told once: the later cross-check gets the honest answer.
        ("I19", NotDetected),
        // Structure of a response.
        ("B01", Refused),
        ("B02", Refused),
        ("B03", Refused),
        ("B04", Refused),
    ];
    let rows = fault_matrix::run();
    let actual: Vec<(&str, Verdict)> = rows.iter().map(|r| (r.id, r.verdict)).collect();
    assert_eq!(actual, expected);

    // Exactly one fault leaves a wrong result that a later cross-check does
    // not show: the one where the source no longer lies when asked again.
    assert_eq!(
        rows.iter()
            .filter(|r| r.verdict == NotDetected)
            .map(|r| r.id)
            .collect::<Vec<_>>(),
        vec!["I19"]
    );
    // The precise shape of the silent failures.
    let alone = |id: &str| rows.iter().find(|r| r.id == id).unwrap().alone.clone();
    let wrong = |missing, invented, altered| Alone::WrongResult {
        missing,
        invented,
        altered,
    };
    assert_eq!(alone("F05"), wrong(1, 0, 0));
    assert_eq!(alone("F06"), wrong(2, 0, 0));
    assert_eq!(alone("I01"), wrong(1, 0, 0));
    assert_eq!(alone("I04"), wrong(0, 0, 1));
    assert_eq!(alone("I06"), wrong(0, 1, 0));

    assert!(matches!(
        alone("F12"),
        Alone::WrongResult { missing, invented, .. } if missing > 0 && invented > 0
    ));
    assert_eq!(alone("I19"), wrong(1, 0, 0));

    // The document shows exactly this table, and counts its rows correctly.
    assert!(
        DOC.contains(&format!("注入したのは {} 種類", rows.len())),
        "docs/FAULT_INJECTION.md states another number of faults than {}",
        rows.len()
    );
    let is_fault_row = |line: &str| {
        let id = line.strip_prefix("| ").and_then(|rest| rest.get(..3));
        id.is_some_and(|id| {
            id.starts_with(['F', 'I', 'B']) && id[1..].bytes().all(|b| b.is_ascii_digit())
        })
    };
    assert_eq!(
        DOC.lines().filter(|l| is_fault_row(l)).count(),
        rows.len(),
        "docs/FAULT_INJECTION.md has rows the matrix does not produce"
    );
    let table = fault_matrix::render_markdown(&rows);
    assert!(
        DOC.contains(&table),
        "docs/FAULT_INJECTION.md is out of date; regenerate its table with \
         `cargo run -p lab-testkit --bin fault_report`:\n{table}"
    );
}

/// Fault I07 in the persistent sync: when the replayed entry and the original
/// fall into different chunks, the scan of each chunk sees nothing wrong —
/// the store's "an output is recorded once" rule is what refuses it.
#[test]
fn a_replayed_index_entry_is_refused_by_the_store_across_chunks() {
    let faulty = ReplayingOracle::start();
    let keys = fixture_gen::lab_receiver_keys();
    let scanner = ReceiveScanner::new(&keys, &[LABEL_USED]).unwrap();
    let mut store = MemoryStore::new();
    store
        .initialize(&new_store_meta(
            &faulty.source,
            &scanner,
            &keys.wallet_id(),
            1,
            "test",
        ))
        .unwrap();
    let result = sync(
        &faulty.source,
        &scanner,
        &keys.wallet_id(),
        &mut store,
        &SyncOptions::default().with_chunk_size(1),
    );
    match result {
        Err(SyncError::Interrupted { cause, partial }) => {
            assert!(
                matches!(*cause, SyncError::Store(StoreError::Invariant(ref m)) if m.contains("recorded twice")),
                "{cause:?}"
            );
            assert_eq!(partial.checkpoint.map(|c| c.height), Some(4));
        }
        other => panic!("expected the replay to be refused, got {other:?}"),
    }
    // Nothing of block 5 was stored.
    assert_eq!(store.checkpoint().unwrap().map(|c| c.height), Some(4));
    assert!(store
        .detections()
        .unwrap()
        .iter()
        .all(|d| d.block.height <= 4));
}
