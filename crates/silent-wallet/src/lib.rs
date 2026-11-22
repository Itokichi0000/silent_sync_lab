//! # silent-wallet
//!
//! **EXPERIMENTAL.** Unaudited research code.
//!
//! Wallet logic for BIP352 (Silent Payments) on top of the `silent-sync`
//! SDK: what a store shows the wallet to hold, and building and signing a
//! transaction that spends it. Regtest and signet by default. With the
//! `mainnet` feature a [`Wallet`] can be made for mainnet, and then this
//! crate signs transactions that move real funds: nobody has audited it, and
//! a mistake in it can lose them.
//!
//! * [`keys`] — the scan and spend private keys, derived from a seed that is
//!   supplied on every start. Never stored, never printed.
//! * [`mod@holdings`] — "unspent" in the only sense this crate uses the word: an
//!   output detected through full blocks with no recorded spend. Outputs an
//!   index claimed are listed apart and never summed.
//! * [`send`] — building and signing. The one module that uses the spend
//!   key; every operation on keys is a call into `bdk_sp`, `bitcoin` or
//!   `secp256k1`. Read its module documentation for what it checks.
//! * [`rpc`] — handing a transaction to one's own node (loopback only).
//! * [`pending`] — transactions that were sent and are not in a block yet.
//!
//! * [`client`] (feature `client`) — all of the above behind a handful of
//!   calls: seed phrases, a store on disk, receive, sync, send. Start here
//!   when embedding a wallet in an application.
//!
//! Scanning, storage, reorganizations, audits and rescans are the SDK's:
//! apart from [`client`], which only wires the pieces together, this crate
//! neither scans ranges nor keeps a store of its own.
#![warn(missing_docs)]

#[cfg(feature = "client")]
pub mod client;
pub mod holdings;
pub mod keys;
pub mod pending;
pub mod rpc;
pub mod send;

pub use holdings::{holdings, Held, Holdings};
pub use keys::{KeyError, Wallet, WalletError, WalletKeys};
pub use pending::{PendingError, PendingSpends, PendingTransaction, Settlement};
pub use rpc::{CoreRpc, RpcError};
pub use send::{build_and_sign, BuiltTransaction, Destination, Payment, SendError, SendLimits};
