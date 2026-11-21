//! # sync-core
//!
//! **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.** Unaudited research code.
//!
//! Receive-side BIP352 (Silent Payments) scanning core for the Silent Sync Lab.
//! The crate is the candidate SDK surface: it has no CLI, printing or storage
//! concerns and never talks to mainnet.
//!
//! All BIP352 cryptography (key extraction, input hash, ECDH, output matching)
//! is delegated to the upstream [`bdk_sp`] crate. This crate only adds
//! orchestration, eligibility pre-checks that inspect scripts, data-source
//! validation and fail-closed error handling.
//!
//! What exists and what does not is tracked in `docs/PROGRESS.md`.

pub mod engine;
pub mod models;
pub mod network;
pub mod protocol;
pub mod source;
pub mod store;
pub mod sync;

/// Re-export of the exact `bitcoin` crate version used by the upstream scanner,
/// so downstream crates cannot accidentally mix incompatible versions.
pub use bdk_sp::bitcoin;

/// Banner every user-facing frontend must show (design spec §5.10, §11).
pub const EXPERIMENTAL_BANNER: &str =
    "EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS (unaudited research code)";
