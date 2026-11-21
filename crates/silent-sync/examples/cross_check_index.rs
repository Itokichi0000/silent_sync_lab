//! Compare a tweak index with full blocks, offline.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! An index is convenient but cannot be verified on its own: it can omit a
//! payment or invent one. `cross_check` scans the same range through both
//! sources and explains every difference. Here both come from the same
//! fixture file, so they agree.
//!
//! ```sh
//! cargo run -p silent-sync --example cross_check_index --features fixture,local-index \
//!     -- fixtures/local/regtest_basic_v1.json
//! ```

use silent_sync::{
    bitcoin::secp256k1::{PublicKey, SecretKey},
    cross_check,
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
    },
    ChainIdentity, Network, ReceiveScanner, ReceiverKeys, ScanSource,
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
        .ok_or("usage: cross_check_index <fixture.json>")?;
    let keys = test_vector_keys()?;
    // Label 1 in addition to the change label, which is always scanned for.
    let scanner = ReceiveScanner::new(&keys, &[1])?;

    let full_blocks = FixtureSource::from_path(Path::new(&path))?;
    let index = LocalTweakIndex::new(
        FixtureSource::from_path(Path::new(&path))?,
        IndexPolicy::Bip352,
    );
    let tip = full_blocks.tip()?.height;

    let report = cross_check(
        &index,
        &full_blocks,
        &scanner,
        ChainIdentity::for_network(Network::Regtest),
        1,
        tip,
    )?;
    println!(
        "heights 1..={tip}: {} outputs agreed, {} only in the index, {} only in the blocks",
        report.agreed, report.candidate_only, report.reference_only
    );
    for difference in &report.differences {
        println!(
            "  height {} {}:{} — {:?}",
            difference.height, difference.txid, difference.vout, difference.explanation
        );
    }
    println!("identical: {}", report.identical);
    Ok(())
}
