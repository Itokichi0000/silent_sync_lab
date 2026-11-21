//! # silent-sync
//!
//! **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.** Unaudited research code.
//!
//! Receive-side synchronisation for BIP352 (Silent Payments): find the
//! outputs that pay a receiver, keep track of how far the chain has been
//! scanned, follow reorganisations, and say what kind of evidence each
//! detection rests on.
//!
//! ## What it does and does not do
//!
//! * **Does**: scan block ranges ([`scan_range`]), keep a resumable,
//!   reorg-aware store up to date ([`sync()`]), compare two data sources
//!   ([`cross_check`]), compare what a store holds with a fresh scan
//!   through a reference source ([`audit_store`]), and scan a store's range
//!   again with more labels into a store that can replace it
//!   ([`rescan_into`]).
//! * **Does not**: send, sign, track spent outputs or balances, or store
//!   keys. A detected output is not a balance.
//! * **Mainnet is not representable**: [`Network`] has no mainnet variant,
//!   and every source refuses a mainnet chain.
//! * All BIP352 cryptography (key extraction, input hash, ECDH, output
//!   matching) is done by the upstream `bdk_sp` crate. This crate adds
//!   orchestration, script-level eligibility checks, validation of what data
//!   sources return, and fail-closed error handling.
//!
//! ## Evidence
//!
//! Every [`DetectedOutput`] says which kind of evidence it came from
//! ([`EvidenceKind`]):
//!
//! * `FullBlockWithPrevouts` — the scanner saw every transaction of the
//!   block together with the outputs it spends, and decided itself which
//!   transactions to scan. What was *verified* about the block depends on
//!   the source and is stated by its
//!   [`SourceCapabilities`](source::SourceCapabilities): the Bitcoin Core
//!   source checks proof of work, merkle root and witness commitment against
//!   a header chain followed from the network's genesis block, which is worth
//!   exactly as much as the certainty that the node it talks to is yours.
//!   Previous outputs created before the scanned range are taken on trust
//!   from any source.
//! * `TweakIndex` — an index server's claim. The transaction id, output
//!   position and amount were **not** checked against the block: such a
//!   server can omit a payment, alter one, or invent one, and nothing here
//!   can tell. Do not treat these detections as confirmed payments.
//!
//! A store filled from an index can be checked afterwards: [`audit_store`]
//! scans the stored range again through a reference — your own node — and
//! reports every output the store lacks, has in addition, or holds with
//! another amount or label. The stored detections keep the evidence kind
//! they were found with; an audit reports, it does not upgrade them.
//!
//! ## Example
//!
//! Scan a range through your own Bitcoin Core node (REST, loopback only) and
//! keep the result in a store. The same code works with any [`SyncStore`];
//! the `silent-sync-sqlite` crate provides a persistent one.
//!
//! ```no_run
//! # #[cfg(feature = "core-rest")]
//! # {
//! use silent_sync::bitcoin::secp256k1::{PublicKey, SecretKey};
//! use silent_sync::source::CoreRestSource;
//! use silent_sync::{
//!     new_store_meta, MemoryStore, ReceiveScanner, ReceiverKeys, SyncOptions, SyncStore,
//! };
//!
//! fn sync_once(
//!     scan_secret: SecretKey,
//!     spend_public: PublicKey,
//! ) -> Result<(), Box<dyn std::error::Error>> {
//!     let keys = ReceiverKeys::new(scan_secret, spend_public);
//!     // No extra labels; the change label is always scanned for.
//!     let scanner = ReceiveScanner::new(&keys, &[])?;
//!     let source = CoreRestSource::connect("http://127.0.0.1:38332")?;
//!
//!     // First use: bind the store to this chain, wallet, backend and birthday.
//!     let mut store = MemoryStore::new();
//!     let birthday = 1;
//!     store.initialize(&new_store_meta(
//!         &source,
//!         &scanner,
//!         birthday,
//!         "the wallet was created after this height",
//!     ))?;
//!
//!     let report = silent_sync::sync(
//!         &source,
//!         &scanner,
//!         &mut store,
//!         &SyncOptions::default(),
//!     )?;
//!     for output in &report.new_detections {
//!         println!(
//!             "{}:{} {} sat ({:?})",
//!             output.txid, output.vout, output.value_sats, output.provenance.evidence_kind
//!         );
//!     }
//!     Ok(())
//! }
//! # }
//! ```
//!
//! ## Features
//!
//! None is enabled by default.
//!
//! | feature | adds |
//! |---|---|
//! | `core-rest` | `source::CoreRestSource`: your own Bitcoin Core node over REST (loopback only) |
//! | `blindbit-http` | `source::BlindbitHttpSource`: a BlindBit Oracle v2 tweak index over HTTP (loopback only) |
//! | `fixture` | `source::fixture`: offline fixture files, for experiments and tests |
//! | `local-index` | `source::local_index`: a tweak index computed locally, for cross-checks |
//! | `low-level` | per-transaction entry points of [`ReceiveScanner`] and their result types |
//! | `spend` | `spend::recover_spendable`: derives again, from a full block, the tweak a wallet needs to spend one detected output. Nothing in this crate signs, sends or stores it |
//! | `secret-output-tweaks` | not part of the SDK; lets conformance tests read per-output tweaks |
//!
//! ## Labels
//!
//! A store is bound to the label set it was scanned with. Blocks already
//! scanned were not checked for a label added later, and an output sent to
//! an unknown label also hides the outputs after it in the same transaction,
//! so adding a label means scanning the whole range again. [`rescan_into`]
//! does that into a second store and says when it is safe to replace the
//! first with it; `silent-sync-sqlite` provides the replacement as one atomic
//! file rename. Keep a backup of the labels in use: a wallet restored without
//! one does not find the payments made to it.
//!
//! ## Implementing your own source or store
//!
//! Implement [`ScanSource`] or [`SyncStore`]. A store must enforce
//! [`store::check_append`] and return detections in the order of
//! [`store::sort_detections`]; the trait documentation states the contract.
//!
//! ## Stability
//!
//! Version 0.x: the API may change. Report and error types are
//! `#[non_exhaustive]`, so new fields and variants are not breaking changes.
#![warn(missing_docs)]

pub mod audit;
pub mod engine;
pub mod models;
pub mod network;
pub mod protocol;
pub mod rescan;
pub mod source;
#[cfg(feature = "spend")]
pub mod spend;
pub mod store;
pub mod sync;
pub mod verify;

/// Re-export of the exact `bitcoin` crate version used by the upstream scanner,
/// so downstream crates cannot accidentally mix incompatible versions.
pub use bdk_sp::bitcoin;

pub use audit::{audit_store, AuditError, AuditOptions, SpendDifference, StoreAuditReport};
pub use engine::{scan_range, ScanError, ScanReport};
pub use models::{BlockRef, DetectedOutput, EvidenceKind, MatchProvenance, SpentOutput};
pub use network::{ChainIdentity, Network, NetworkError};
pub use protocol::{ProtocolError, ReceiveScanner, ReceiverKeys, SilentPaymentAddress};
pub use rescan::{rescan_into, RescanError, RescanReport};
pub use source::{ScanSource, SourceError};
pub use store::{MemoryStore, StoreError, StoreMeta, SyncStore};
pub use sync::{
    new_store_meta, sync, sync_with_progress, ReorgReport, StopReason, SyncError, SyncOptions,
    SyncReport,
};
pub use verify::{cross_check, Difference, Explanation, Side, VerificationReport};
