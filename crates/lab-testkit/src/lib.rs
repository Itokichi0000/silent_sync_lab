//! TEST-ONLY helpers. Nothing here may be used by `silent-sync` or by the
//! `silent-sync` binary: this crate is where upstream's *sending* functions
//! and transaction signing live, so that the lab itself stays receive-only.
//!
//! * [`fixture_gen`] — deterministic offline fixture chain.
//! * [`fake_http`] — in-process stand-ins for Bitcoin Core's REST interface
//!   and for BlindBit Oracle's HTTP interface that can be made to lie.
//! * [`test_source`] / [`sync_suite`] — a scriptable source and the behavioural
//!   suite every `SyncStore` implementation must pass.
//! * [`regtest_node`] — start/stop a throwaway Bitcoin Core regtest node.
//! * [`regtest_wallet`] — fund test keys and build signed silent payment
//!   transactions on that node.

pub mod fake_http;
pub mod fault_matrix;
pub mod fixture_gen;
pub mod oracle_node;
pub mod regtest_node;
pub mod regtest_wallet;
pub mod sync_suite;
pub mod test_source;
pub mod test_store;
