//! What a store shows the wallet to hold.
//!
//! "Unspent" has a narrow meaning here, and the types keep it narrow: an
//! output that was detected **through full blocks** and for which no spend
//! is recorded up to the store's checkpoint. Outputs an index server claimed
//! are listed separately and never added to a total — for them nothing was
//! checked against the chain, and nothing shows whether they were spent.
//! Transactions that are not in a block yet are not seen at all.

use crate::pending::PendingSpends;
use serde::Serialize;
use silent_sync::{
    bitcoin::OutPoint, BlockRef, DetectedOutput, EvidenceKind, StoreError, SyncStore,
};
use std::collections::BTreeSet;

/// A detected output with the number of blocks that confirm it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Held {
    /// The output.
    pub output: DetectedOutput,
    /// Blocks from the one that contains it up to the store's checkpoint,
    /// both included (1 = in the checkpoint block itself).
    pub confirmations: u32,
}

/// What the store shows as of its checkpoint. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Holdings {
    /// The checkpoint this is a statement about. `None`: nothing scanned yet.
    pub as_of: Option<BlockRef>,
    /// Confirmations an output needs to be listed in `unspent`.
    pub min_confirmations: u32,
    /// Detected through full blocks, no spend recorded, enough
    /// confirmations, and not an input of a transaction this wallet sent
    /// that is still waiting for a block.
    pub unspent: Vec<Held>,
    /// Like `unspent`, but with fewer confirmations than asked for.
    pub unconfirmed_enough: Vec<Held>,
    /// Inputs of transactions this wallet sent that are not in a scanned
    /// block yet: not counted as unspent, not recorded as spent.
    pub spent_by_pending: Vec<Held>,
    /// Number of detected outputs for which a spend is recorded.
    pub spent: u64,
    /// Outputs an index claimed. Not verified against the chain, spent
    /// status unknown, never part of a total.
    pub index_claims: Vec<DetectedOutput>,
}

impl Holdings {
    /// Sum of `unspent`, in satoshi: outputs detected through full blocks,
    /// without a recorded spend, with at least `min_confirmations`.
    pub fn unspent_sats(&self) -> u64 {
        self.unspent.iter().map(|h| h.output.value_sats).sum()
    }
}

/// Reads what `store` holds. `pending` are the inputs of transactions this
/// wallet sent that have not been seen in a block.
pub fn holdings(
    store: &dyn SyncStore,
    min_confirmations: u32,
    pending: &PendingSpends,
) -> Result<Holdings, StoreError> {
    let as_of = store.checkpoint()?;
    let spent: BTreeSet<OutPoint> = store
        .spends()?
        .iter()
        .map(|s| OutPoint::new(s.txid, s.vout))
        .collect();
    let waiting = pending.inputs();
    let mut result = Holdings {
        as_of,
        min_confirmations,
        unspent: Vec::new(),
        unconfirmed_enough: Vec::new(),
        spent_by_pending: Vec::new(),
        spent: 0,
        index_claims: Vec::new(),
    };
    let Some(tip) = as_of else {
        return Ok(result);
    };
    for output in store.detections()? {
        let outpoint = OutPoint::new(output.txid, output.vout);
        if output.provenance.evidence_kind != EvidenceKind::FullBlockWithPrevouts {
            result.index_claims.push(output);
            continue;
        }
        if spent.contains(&outpoint) {
            result.spent += 1;
            continue;
        }
        let confirmations = tip.height.saturating_sub(output.block.height) + 1;
        let held = Held {
            output,
            confirmations,
        };
        if waiting.contains(&outpoint) {
            result.spent_by_pending.push(held);
        } else if confirmations >= min_confirmations {
            result.unspent.push(held);
        } else {
            result.unconfirmed_enough.push(held);
        }
    }
    Ok(result)
}
