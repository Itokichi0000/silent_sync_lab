//! TEST-ONLY helpers. Nothing here may be used by `sync-core` or by the
//! `silent-sync` binary: this crate is where upstream's *sending* functions
//! and transaction signing live, so that the lab itself stays receive-only.
//!
//! * [`fixture_gen`] — deterministic offline fixture chain.
//! * [`fake_core_rest`] — an in-process stand-in for Bitcoin Core's REST
//!   interface that can be made to lie.
//! * [`regtest_node`] — start/stop a throwaway Bitcoin Core regtest node.
//! * [`regtest_wallet`] — fund test keys and build signed silent payment
//!   transactions on that node.

pub mod fake_core_rest;
pub mod fixture_gen;
pub mod regtest_node;
pub mod regtest_wallet;
