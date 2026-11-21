//! `silent-sync` with default features, seen from outside.
//!
//! Each "must not compile" example is paired with a control that differs only
//! in the forbidden item, so that the example cannot pass for an unrelated
//! reason (a typo, a missing import).
//!
//! ## The entry points are there
//!
//! ```
//! use silent_sync::{
//!     audit_store, cross_check, new_store_meta, rescan_into, scan_range, sync,
//!     sync_with_progress, AuditError, AuditOptions, BlockRef, ChainIdentity, DetectedOutput,
//!     EvidenceKind, MemoryStore, Network, ReceiveScanner, ReceiverKeys, RescanError,
//!     RescanReport, ScanError, ScanSource, SilentPaymentAddress, StoreAuditReport, StoreError,
//!     StoreMeta, SyncError, SyncOptions, SyncReport, SyncStore, VerificationReport,
//! };
//! # let _ = (scan_range, sync, sync_with_progress, cross_check, new_store_meta, audit_store);
//! # let _ = rescan_into;
//! # fn audit(_: (AuditError, AuditOptions, StoreAuditReport, RescanError, RescanReport)) {}
//! # fn names(_: (BlockRef, ChainIdentity, DetectedOutput, EvidenceKind, MemoryStore, Network)) {}
//! # fn more(_: (ScanError, StoreError, StoreMeta, SyncError, SyncOptions, SyncReport)) {}
//! # fn rest(_: (&ReceiveScanner, &ReceiverKeys, &SilentPaymentAddress, &dyn ScanSource)) {}
//! # fn last(_: (&dyn SyncStore, &VerificationReport)) {}
//! ```
//!
//! ## Per-transaction scanning is not
//!
//! Control:
//! ```
//! use silent_sync::bitcoin::{Transaction, TxOut};
//! use silent_sync::ReceiveScanner;
//! fn f(scanner: &ReceiveScanner, _tx: &Transaction, _prevouts: &[TxOut]) {
//!     let _ = scanner.labels().count();
//! }
//! ```
//! ```compile_fail
//! use silent_sync::bitcoin::{Transaction, TxOut};
//! use silent_sync::ReceiveScanner;
//! fn f(scanner: &ReceiveScanner, tx: &Transaction, prevouts: &[TxOut]) {
//!     let _ = scanner.scan_tx(tx, prevouts);
//! }
//! ```
//! ```compile_fail
//! use silent_sync::bitcoin::{Transaction, TxOut};
//! use silent_sync::ReceiveScanner;
//! fn f(tx: &Transaction, prevouts: &[TxOut]) {
//!     let _ = ReceiveScanner::public_tweak_data(tx, prevouts);
//! }
//! ```
//! ```compile_fail
//! use silent_sync::bitcoin::{Transaction, TxOut};
//! use silent_sync::ReceiveScanner;
//! fn f(tx: &Transaction, prevouts: &[TxOut]) {
//!     let _ = ReceiveScanner::eligibility(tx, prevouts);
//! }
//! ```
//! ```compile_fail
//! use silent_sync::{ReceiveScanner, ReceiverKeys};
//! fn f(keys: &ReceiverKeys) {
//!     let _ = ReceiveScanner::with_exact_labels(keys, &[]);
//! }
//! ```
//! Control for the one above:
//! ```
//! use silent_sync::{ReceiveScanner, ReceiverKeys};
//! fn f(keys: &ReceiverKeys) {
//!     let _ = ReceiveScanner::new(keys, &[]);
//! }
//! ```
//!
//! ## Its result types, and with them the per-output tweak, cannot be named
//!
//! Control:
//! ```
//! fn f(_: &silent_sync::protocol::ReceiveScanner) {}
//! ```
//! ```compile_fail
//! fn f(_: &silent_sync::protocol::ScanMatch) {}
//! ```
//! ```compile_fail
//! fn f(_: &silent_sync::protocol::TxScanOutcome) {}
//! ```
//! ```compile_fail
//! fn f(_: silent_sync::protocol::SkipReason) {}
//! ```
//!
//! ## Experimental sources and network backends are absent
//!
//! Control:
//! ```
//! use silent_sync::source::ScanSource;
//! ```
//! ```compile_fail
//! use silent_sync::source::fixture::FixtureSource;
//! ```
//! ```compile_fail
//! use silent_sync::source::local_index::LocalTweakIndex;
//! ```
//! ```compile_fail
//! use silent_sync::source::CoreRestSource;
//! ```
//! ```compile_fail
//! use silent_sync::source::BlindbitHttpSource;
//! ```
//!
//! ## Options cannot be built field by field, keys cannot be read back
//!
//! Control:
//! ```
//! let _ = silent_sync::SyncOptions::default().with_chunk_size(10);
//! ```
//! ```compile_fail
//! let _ = silent_sync::SyncOptions {
//!     chunk_size: 10,
//!     target_height: None,
//!     max_blocks: None,
//!     max_reorg_depth: None,
//! };
//! ```
//! ```compile_fail
//! fn f(keys: &silent_sync::ReceiverKeys) {
//!     let _ = keys.scan_sk;
//! }
//! ```
//! Control:
//! ```
//! fn f(keys: &silent_sync::ReceiverKeys) {
//!     let _ = keys.scan_public_key();
//! }
//! ```

#[cfg(test)]
mod tests {
    use silent_sync::{
        audit_store,
        bitcoin::secp256k1::{Secp256k1, SecretKey},
        models::BlockRef,
        network::ChainIdentity,
        rescan_into,
        source::{ScanBatch, ScanSource, SourceCapabilities, SourceError},
        store::{MemoryStore, StoreError, SyncStore},
        sync::{new_store_meta, sync, SyncError, SyncOptions},
        AuditError, AuditOptions, EvidenceKind, Network, ReceiveScanner, ReceiverKeys, RescanError,
    };

    /// A source an embedder might write: here one that cannot be reached.
    struct Unreachable;

    impl ScanSource for Unreachable {
        fn backend_id(&self) -> &str {
            "probe"
        }
        fn capabilities(&self) -> SourceCapabilities {
            SourceCapabilities {
                evidence_kind: EvidenceKind::FullBlockWithPrevouts,
                verifies_merkle_root: false,
                verifies_witness_commitment: false,
                verifies_proof_of_work: false,
                consensus_validated: false,
                remote: false,
            }
        }
        fn chain(&self) -> ChainIdentity {
            ChainIdentity::for_network(Network::Regtest)
        }
        fn tip(&self) -> Result<BlockRef, SourceError> {
            Err(SourceError::Unavailable("probe".to_string()))
        }
        fn fetch_range(&self, _from: u32, _to: u32) -> Result<ScanBatch, SourceError> {
            Err(SourceError::Unavailable("probe".to_string()))
        }
    }

    /// Keys made up for this probe: they never hold anything.
    fn keys() -> ReceiverKeys {
        let secp = Secp256k1::new();
        let scan = SecretKey::from_slice(&[0x21; 32]).expect("valid");
        let spend = SecretKey::from_slice(&[0x22; 32])
            .expect("valid")
            .public_key(&secp);
        ReceiverKeys::new(scan, spend)
    }

    /// The default-feature library, actually run: an outside source and the
    /// in-memory store go through `sync`, which fails closed.
    #[test]
    fn an_outside_source_and_store_work_with_default_features() {
        let keys = keys();
        let scanner = ReceiveScanner::new(&keys, &[7]).expect("scanner");
        assert_eq!(scanner.labels().collect::<Vec<_>>(), vec![0, 7]);
        let source = Unreachable;
        let mut store = MemoryStore::new();

        // Not initialized: refused before the source is asked anything.
        let audit = |store: &MemoryStore| {
            audit_store(
                &source,
                &scanner,
                &keys.wallet_id(),
                store,
                &AuditOptions::default(),
            )
        };
        assert_eq!(
            audit(&store),
            Err(AuditError::Store(StoreError::NotInitialized))
        );
        let options = SyncOptions::default().with_chunk_size(10);
        assert!(matches!(
            sync(&source, &scanner, &keys.wallet_id(), &mut store, &options),
            Err(SyncError::Store(StoreError::NotInitialized))
        ));
        store
            .initialize(&new_store_meta(
                &source,
                &scanner,
                &keys.wallet_id(),
                1,
                "probe",
            ))
            .expect("initialize");
        // The source is down: an error, and nothing is recorded.
        assert!(sync(&source, &scanner, &keys.wallet_id(), &mut store, &options).is_err());
        assert_eq!(store.checkpoint().expect("checkpoint"), None);
        assert!(store.detections().expect("detections").is_empty());
        // Nothing stored: an audit has nothing to compare, and says so.
        assert_eq!(audit(&store), Err(AuditError::NothingStored));
        // A rescan with fewer labels than the store was scanned with is
        // refused before the fresh store is touched.
        let fewer = ReceiveScanner::new(&keys, &[]).expect("scanner");
        let mut fresh = MemoryStore::new();
        assert!(matches!(
            rescan_into(
                &source,
                &fewer,
                &keys.wallet_id(),
                &store,
                &mut fresh,
                &options
            ),
            Err(RescanError::LabelsRemoved { .. })
        ));
        assert_eq!(fresh.meta().expect("meta"), None);
    }

    #[test]
    fn mainnet_cannot_be_named_and_keys_are_not_printed() {
        assert!("mainnet".parse::<Network>().is_err());
        assert!("bitcoin".parse::<Network>().is_err());
        assert_eq!("regtest".parse::<Network>().ok(), Some(Network::Regtest));
        let keys = keys();
        let shown = format!(
            "{keys:?} {:?}",
            ReceiveScanner::new(&keys, &[]).expect("scanner")
        );
        assert!(shown.contains("<redacted>"));
        assert!(!shown.contains(&"21".repeat(32)), "the scan key is printed");
    }
}
