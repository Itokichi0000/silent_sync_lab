//! # silent-wallet
//!
//! **EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.** Unaudited research code.
//!
//! Wallet logic for BIP352 (Silent Payments) on top of the `silent-sync`
//! SDK: what a store shows the wallet to hold, and building and signing a
//! transaction that spends it. Regtest and signet only — mainnet cannot be
//! named in the types this crate uses.
//!
//! * [`keys`] — the scan and spend private keys, derived from a seed that is
//!   supplied on every start. Never stored, never printed.
//! * [`holdings`] — "unspent" in the only sense this crate uses the word: an
//!   output detected through full blocks with no recorded spend. Outputs an
//!   index claimed are listed apart and never summed.
//! * [`send`] — building and signing. The one module that uses the spend
//!   key; every operation on keys is a call into `bdk_sp`, `bitcoin` or
//!   `secp256k1`. Read its module documentation for what it checks.
//! * [`rpc`] — handing a transaction to one's own node (loopback only).
//! * [`pending`] — transactions that were sent and are not in a block yet.
//!
//! Scanning, storage, reorganizations, audits and rescans are the SDK's:
//! this crate neither scans ranges nor keeps a store of its own.
#![warn(missing_docs)]

pub mod holdings;
pub mod keys;
pub mod pending;
pub mod rpc;
pub mod send;

pub use holdings::{holdings, Held, Holdings};
pub use keys::{KeyError, Wallet, WalletKeys};
pub use pending::{PendingError, PendingSpends, PendingTransaction, Settlement};
pub use rpc::{CoreRpc, RpcError};
pub use send::{build_and_sign, BuiltTransaction, Destination, Payment, SendError, SendLimits};
