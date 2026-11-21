//! `silent-sync` with the `low-level` feature, seen from outside: the
//! per-transaction API is available, and the per-output tweak still is not
//! (DECISIONS D-039: the library does not hand out that secret).
//!
//! ## Available
//!
//! ```
//! use silent_sync::bitcoin::{Transaction, TxOut};
//! use silent_sync::protocol::{ReceiveScanner, ScanMatch, SkipReason, TxScanOutcome};
//! fn f(scanner: &ReceiveScanner, tx: &Transaction, prevouts: &[TxOut]) {
//!     let _ = scanner.scan_tx(tx, prevouts);
//!     let _ = ReceiveScanner::public_tweak_data(tx, prevouts);
//!     let _ = ReceiveScanner::eligibility(tx, prevouts);
//! }
//! fn g(found: &ScanMatch, outcome: &TxScanOutcome, reason: SkipReason) {
//!     let _ = (found.outpoint, found.amount, found.label, outcome, reason.as_str());
//! }
//! ```
//!
//! ## Not available: the tweak
//!
//! Control:
//! ```
//! fn f(found: &silent_sync::protocol::ScanMatch) {
//!     let _ = found.outpoint;
//! }
//! ```
//! ```compile_fail
//! fn f(found: &silent_sync::protocol::ScanMatch) {
//!     let _ = found.sensitive_output_tweak();
//! }
//! ```
//! ```compile_fail
//! fn f(found: &silent_sync::protocol::ScanMatch) {
//!     let _ = found.tweak;
//! }
//! ```
//!
//! ## A match cannot be built or taken apart exhaustively
//!
//! (so that enabling `secret-output-tweaks` elsewhere in a build, which adds
//! a private field, cannot break code written against this feature set)
//!
//! Control:
//! ```
//! fn f(found: &silent_sync::protocol::ScanMatch) {
//!     let silent_sync::protocol::ScanMatch { outpoint, .. } = found;
//!     let _ = outpoint;
//! }
//! ```
//! ```compile_fail
//! fn f(found: &silent_sync::protocol::ScanMatch) {
//!     let silent_sync::protocol::ScanMatch { outpoint, script_pubkey, amount, label } = found;
//!     let _ = (outpoint, script_pubkey, amount, label);
//! }
//! ```
