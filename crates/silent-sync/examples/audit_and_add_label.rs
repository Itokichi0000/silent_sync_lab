//! Check a store against full blocks, then add a label — offline.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! 1. A store is filled from a tweak index. What it holds is the index's
//!    claim.
//! 2. `audit_store` scans the stored range again through full blocks and
//!    compares the result with the store: this finds what an index omitted,
//!    altered or invented while the store was being filled.
//! 3. A label is added. Blocks already scanned were not checked for it, so
//!    `rescan_into` scans the whole range again into a second store, and
//!    says when that store may replace the first. Replacing is the caller's
//!    step; with `silent-sync-sqlite` it is one atomic file rename
//!    (`replace_store_file`).
//!
//! ```sh
//! cargo run -p silent-sync --example audit_and_add_label --features fixture,local-index \
//!     -- fixtures/local/regtest_basic_v1.json
//! ```

use silent_sync::{
    audit_store,
    bitcoin::secp256k1::{PublicKey, SecretKey},
    new_store_meta, rescan_into,
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
    },
    sync, AuditOptions, MemoryStore, ReceiveScanner, ReceiverKeys, SyncOptions, SyncStore,
};
use std::{error::Error, path::Path};

/// The receiver of BIP352 test vector #0: a published test key that the
/// fixture's payments are addressed to. Never use it for anything of value.
fn test_vector_keys() -> Result<ReceiverKeys, Box<dyn Error>> {
    let scan: SecretKey =
        "0f694e068028a717f8af6b9411f9a133dd3565258714cc226594b34db90c1f2c".parse()?;
    let spend: PublicKey =
        "025cc9856d6f8375350e123978daac200c260cb5b5ae83106cab90484dcd8fcf36".parse()?;
    Ok(ReceiverKeys::new(scan, spend))
}

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: audit_and_add_label <fixture.json>")?;
    let keys = test_vector_keys()?;
    let wallet_id = keys.wallet_id();
    let full_blocks = FixtureSource::from_path(Path::new(&path))?;
    let index = LocalTweakIndex::new(
        FixtureSource::from_path(Path::new(&path))?,
        IndexPolicy::Bip352,
    );

    // 1. Fill a store from the index, scanning for label 1.
    let scanner = ReceiveScanner::new(&keys, &[1])?;
    let mut store = MemoryStore::new();
    store.initialize(&new_store_meta(
        &index,
        &scanner,
        &wallet_id,
        1,
        "the fixture chain starts at height 1",
    ))?;
    sync(
        &index,
        &scanner,
        &wallet_id,
        &mut store,
        &SyncOptions::default(),
    )?;
    println!(
        "synced from the index: {} outputs",
        store.detections()?.len()
    );

    // 2. Audit what was stored against full blocks.
    let audit = audit_store(
        &full_blocks,
        &scanner,
        &wallet_id,
        &store,
        &AuditOptions::default(),
    )?;
    println!(
        "audit against full blocks: identical: {} ({} agreed, {} only in the store, {} only \
         in the blocks)",
        audit.identical, audit.agreed, audit.stored_only, audit.reference_only
    );

    // 3. Add label 5: scan the range again into a second store.
    let more_labels = ReceiveScanner::new(&keys, &[1, 5])?;
    let mut fresh = MemoryStore::new();
    let rescan = rescan_into(
        &index,
        &more_labels,
        &wallet_id,
        &store,
        &mut fresh,
        &SyncOptions::default(),
    )?;
    println!(
        "rescan with labels {:?}: {} found again, {} added",
        rescan.labels,
        rescan.reproduced,
        rescan.added.len()
    );
    for output in &rescan.added {
        println!(
            "  height {} {}:{} {} sat, label {:?}, evidence {:?}",
            output.block.height,
            output.txid,
            output.vout,
            output.value_sats,
            output.label,
            output.provenance.evidence_kind
        );
    }
    // `Ok` means the fresh store holds everything the old one held: it may
    // take its place now.
    store = fresh;

    let audit = audit_store(
        &full_blocks,
        &more_labels,
        &wallet_id,
        &store,
        &AuditOptions::default(),
    )?;
    println!(
        "audit after the rescan: identical: {} ({} agreed)",
        audit.identical, audit.agreed
    );
    Ok(())
}
