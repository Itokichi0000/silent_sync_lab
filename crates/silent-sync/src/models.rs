//! Data contracts shared by sources, the engine and frontends.
//!
//! Nothing in this module contains key material. In particular
//! [`DetectedOutput`] deliberately omits the per-output tweak: the lab is
//! receive-only and does not need it.

use crate::{
    bitcoin::{BlockHash, Txid},
    network::Network,
};
use serde::{Deserialize, Serialize};

/// A block position that always carries its hash and its parent hash, so that
/// continuity can be checked wherever a height is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlockRef {
    pub height: u32,
    pub hash: BlockHash,
    pub prev_hash: BlockHash,
}

/// What kind of data a detection was derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Full transactions plus the previous outputs of every input, so the
    /// client itself derived the BIP352 public tweak data.
    FullBlockWithPrevouts,
    /// Per-transaction public tweak data and taproot outputs prepared by an
    /// index. The client did **not** see the transactions' inputs: which
    /// transactions are listed, and with which tweak, is the index's claim.
    TweakIndex,
}

/// Where a detection came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MatchProvenance {
    pub backend_id: String,
    pub evidence_kind: EvidenceKind,
}

/// A transaction output that cryptographically matched the receiver keys.
///
/// "Detected" is *not* "unspent" and is *not* a balance: spent status is not
/// tracked (design spec §2, §6).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DetectedOutput {
    pub network: Network,
    pub block: BlockRef,
    pub txid: Txid,
    pub vout: u32,
    /// Value as stated by the transaction data the source supplied.
    pub value_sats: u64,
    /// BIP352 label integer `m` if the output was sent to a labelled address
    /// (`0` is the change label).
    pub label: Option<u32>,
    pub provenance: MatchProvenance,
}
