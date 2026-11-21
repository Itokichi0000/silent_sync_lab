//! Keep a store on disk in sync with your own Bitcoin Core node.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS. Regtest and signet only.
//!
//! The node must run on this machine with `-rest=1`; only loopback URLs of
//! the form `http://127.0.0.1:<port>` are accepted. The keys are read from
//! the environment so that they never appear on a command line:
//!
//! ```sh
//! SCAN_SECRET_KEY=<hex> SPEND_PUBLIC_KEY=<hex> \
//!     cargo run -p silent-sync-sqlite --example sync_from_node \
//!     -- http://127.0.0.1:18443 wallet.sqlite3 1
//! ```
//!
//! Run it again at any time: it resumes from the stored checkpoint, and if
//! the chain was reorganized it rolls back and reports what it removed.

use silent_sync::{
    bitcoin::secp256k1::{PublicKey, SecretKey},
    new_store_meta,
    source::CoreRestSource,
    sync_with_progress, ReceiveScanner, ReceiverKeys, SyncError, SyncOptions, SyncReport,
    SyncStore,
};
use silent_sync_sqlite::SqliteStore;
use std::{error::Error, path::Path};

fn keys_from_environment() -> Result<ReceiverKeys, Box<dyn Error>> {
    // Parse errors are replaced by a fixed message: they must not echo key
    // material.
    let scan: SecretKey = std::env::var("SCAN_SECRET_KEY")
        .map_err(|_| "set SCAN_SECRET_KEY")?
        .parse()
        .map_err(|_| "SCAN_SECRET_KEY is not a valid key")?;
    let spend: PublicKey = std::env::var("SPEND_PUBLIC_KEY")
        .map_err(|_| "set SPEND_PUBLIC_KEY")?
        .parse()
        .map_err(|_| "SPEND_PUBLIC_KEY is not a valid key")?;
    Ok(ReceiverKeys::new(scan, spend))
}

fn show(report: &SyncReport) {
    if let Some(reorg) = &report.reorg {
        println!(
            "reorganization: rolled back {} blocks to height {}, {} detections removed",
            reorg.blocks_rolled_back,
            reorg.fork_height,
            reorg.detections_removed.len()
        );
    }
    println!(
        "{} blocks scanned, {} new outputs, stopped because {:?}",
        report.blocks_scanned,
        report.new_detections.len(),
        report.stopped_because
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let usage = "usage: sync_from_node <rest-url> <store-file> <birthday-height>";
    let url = args.next().ok_or(usage)?;
    let store_path = args.next().ok_or(usage)?;
    let birthday: u32 = args.next().ok_or(usage)?.parse()?;

    let keys = keys_from_environment()?;
    let scanner = ReceiveScanner::new(&keys, &[])?;
    let source = CoreRestSource::connect(&url)?;

    let path = Path::new(&store_path);
    let mut store = if path.exists() {
        SqliteStore::open(path)?
    } else {
        // The birthday is fixed here: payments confirmed below it will never
        // be found by this store.
        let mut store = SqliteStore::create(path)?;
        store.initialize(&new_store_meta(
            &source,
            &scanner,
            birthday,
            "given on the command line",
        ))?;
        store
    };

    let result = sync_with_progress(
        &source,
        &scanner,
        &mut store,
        &SyncOptions::default(),
        &mut |checkpoint| println!("committed up to height {}", checkpoint.height),
    );
    match result {
        Ok(report) => show(&report),
        // The store was changed before the error: say what was changed.
        Err(SyncError::Interrupted { cause, partial }) => {
            show(&partial);
            return Err(format!("interrupted: {cause}; run again to continue").into());
        }
        Err(e) => return Err(e.into()),
    }

    println!("{} outputs detected in total", store.detections()?.len());
    println!("detected outputs are not a balance: spent status is not tracked");
    Ok(())
}
