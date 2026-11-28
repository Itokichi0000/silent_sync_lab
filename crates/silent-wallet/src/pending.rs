//! Transactions this wallet sent that are not in a scanned block yet.
//!
//! The store only knows blocks. Between sending a transaction and seeing it
//! in a block, its inputs would still look unspent; this small record keeps
//! them out of the next transaction. It holds transaction ids and outpoints
//! — public once the transaction is broadcast — and no key material.

use serde::{Deserialize, Serialize};
use silent_sync::{
    bitcoin::{OutPoint, Txid},
    SpentOutput,
};
use std::{collections::BTreeSet, io::Write as _, path::Path};

/// How long a confirmed transaction stays on record: as deep as a
/// reorganization is followed by default. Until then the block that contains
/// it can still be abandoned, and the transaction is pending again.
pub const KEEP_CONFIRMED_FOR: u32 = silent_sync::sync::DEFAULT_MAX_REORG_DEPTH;

/// One transaction that was handed to the node — or may have been.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransaction {
    /// Its id.
    pub txid: Txid,
    /// The outputs it spends.
    pub inputs: Vec<OutPoint>,
    /// Height of the store's checkpoint when it was sent.
    pub sent_at_height: u32,
    /// Height of the block the store saw it in, once it has.
    #[serde(default)]
    pub confirmed_at: Option<u32>,
}

/// What a scan changed for a transaction on record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    /// The store now records all its inputs as spent by it.
    Confirmed,
    /// The block that contained it was abandoned: it is waiting again, and
    /// its inputs must not be offered to another transaction.
    Unconfirmed,
    /// Another transaction spent one of its inputs: it can no longer
    /// confirm. Dropped from the record.
    Conflicted,
}

/// The transactions sent and not yet buried deep enough to forget.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSpends {
    transactions: Vec<PendingTransaction>,
    /// Outputs the user has set aside: never offered to a transaction.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    held_back: BTreeSet<OutPoint>,
}

/// Why the record could not be read or written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PendingError {
    #[error("cannot access {path}: {reason}")]
    /// The file could not be read or written.
    Io {
        /// The file.
        path: String,
        /// Why.
        reason: String,
    },
    #[error("{path} is not a record of pending transactions: {reason}")]
    /// The file does not hold what this module writes.
    Malformed {
        /// The file.
        path: String,
        /// Why.
        reason: String,
    },
}

impl PendingSpends {
    /// The transactions on record.
    pub fn transactions(&self) -> &[PendingTransaction] {
        &self.transactions
    }

    /// Every outpoint spent by a recorded transaction that is not in a
    /// block: these must not be offered to another transaction. (Inputs of
    /// confirmed transactions are recorded as spent by the store itself.)
    pub fn inputs(&self) -> BTreeSet<OutPoint> {
        self.transactions
            .iter()
            .filter(|t| t.confirmed_at.is_none())
            .flat_map(|t| t.inputs.iter().copied())
            .collect()
    }

    /// Outputs set aside by the user: they are not offered to a transaction
    /// until released. For an output you do not want to spend together with
    /// your others — for example an unrequested small payment from someone
    /// who would then see which outputs are yours.
    pub fn held_back(&self) -> &BTreeSet<OutPoint> {
        &self.held_back
    }

    /// The first of `outpoints` that must not be spent now: spent by a
    /// recorded transaction not yet in a block, or held back. A transaction
    /// built before the record was read last is checked with this before it
    /// is sent — the hold or the other spend may have come after it.
    pub fn first_unavailable<'a>(
        &self,
        outpoints: impl IntoIterator<Item = &'a OutPoint>,
    ) -> Option<OutPoint> {
        let taken = self.inputs();
        outpoints
            .into_iter()
            .find(|o| taken.contains(o) || self.held_back.contains(o))
            .copied()
    }

    /// Sets an output aside. Returns whether that changed anything.
    pub fn hold_back(&mut self, outpoint: OutPoint) -> bool {
        self.held_back.insert(outpoint)
    }

    /// Releases an output that was set aside. Returns whether it was.
    pub fn release(&mut self, outpoint: OutPoint) -> bool {
        self.held_back.remove(&outpoint)
    }

    /// Records a transaction that is about to be handed to the node.
    pub fn record(&mut self, transaction: PendingTransaction) {
        self.transactions.retain(|t| t.txid != transaction.txid);
        self.transactions.push(transaction);
    }

    /// Brings the record in line with what the store shows at `checkpoint`:
    ///
    /// * a transaction all of whose inputs are recorded as spent by it is
    ///   confirmed; it stays on record until it is [`KEEP_CONFIRMED_FOR`]
    ///   blocks deep, because its block can still be abandoned;
    /// * a confirmed transaction whose spends are no longer recorded (a
    ///   reorganization) is unconfirmed again;
    /// * a transaction one of whose inputs was spent by another transaction
    ///   is dropped.
    ///
    /// Returns what changed.
    pub fn settle(
        &mut self,
        spends: &[SpentOutput],
        checkpoint: u32,
    ) -> Vec<(PendingTransaction, Settlement)> {
        let mut changes = Vec::new();
        let mut kept = Vec::with_capacity(self.transactions.len());
        for mut t in std::mem::take(&mut self.transactions) {
            let recorded: Vec<&SpentOutput> = spends
                .iter()
                .filter(|s| t.inputs.contains(&OutPoint::new(s.txid, s.vout)))
                .collect();
            if recorded.iter().any(|s| s.spending_txid != t.txid) {
                changes.push((t, Settlement::Conflicted));
                continue;
            }
            let in_block = (!t.inputs.is_empty() && recorded.len() == t.inputs.len())
                .then(|| recorded.iter().map(|s| s.spent_in.height).max())
                .flatten();
            match (t.confirmed_at, in_block) {
                (before, Some(height)) => {
                    t.confirmed_at = Some(height);
                    if before != Some(height) {
                        changes.push((t.clone(), Settlement::Confirmed));
                    }
                    if checkpoint.saturating_sub(height) >= KEEP_CONFIRMED_FOR {
                        continue;
                    }
                }
                (Some(_), None) => {
                    t.confirmed_at = None;
                    changes.push((t.clone(), Settlement::Unconfirmed));
                }
                (None, None) => {}
            }
            kept.push(t);
        }
        self.transactions = kept;
        changes
    }

    /// Forgets a transaction (one that will never confirm). Returns whether
    /// it was on record.
    pub fn forget(&mut self, txid: Txid) -> bool {
        let before = self.transactions.len();
        self.transactions.retain(|t| t.txid != txid);
        self.transactions.len() != before
    }

    /// Reads the record at `path`; a missing file is an empty record.
    pub fn load(path: &Path) -> Result<Self, PendingError> {
        let io = |e: std::io::Error| PendingError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        };
        use std::io::Read as _;
        let mut text = String::new();
        match std::fs::File::open(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(io(e)),
            // The record is small; never read an unbounded file.
            Ok(file) => file
                .take(4 * 1024 * 1024)
                .read_to_string(&mut text)
                .map_err(io)?,
        };
        serde_json::from_str(&text).map_err(|e| PendingError::Malformed {
            path: path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// Writes the record to `path`: to a new file next to it, which then
    /// takes its place, so that a crash leaves the old or the new record.
    pub fn save(&self, path: &Path) -> Result<(), PendingError> {
        let io = |e: std::io::Error| PendingError::Io {
            path: path.display().to_string(),
            reason: e.to_string(),
        };
        let text = serde_json::to_string_pretty(self).map_err(|e| PendingError::Malformed {
            path: path.display().to_string(),
            reason: e.to_string(),
        })?;
        let mut temporary = path.as_os_str().to_os_string();
        temporary.push(".new");
        let temporary = std::path::PathBuf::from(temporary);
        let _ = std::fs::remove_file(&temporary);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary).map_err(io)?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(io)?;
        std::fs::rename(&temporary, path).map_err(io)
    }
}
