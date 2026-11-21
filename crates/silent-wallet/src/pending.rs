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

/// One transaction that was handed to the node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTransaction {
    /// Its id.
    pub txid: Txid,
    /// The outputs it spends.
    pub inputs: Vec<OutPoint>,
    /// Height of the store's checkpoint when it was sent.
    pub sent_at_height: u32,
}

/// The transactions sent and not yet seen in a block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSpends {
    transactions: Vec<PendingTransaction>,
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

    /// Every outpoint a recorded transaction spends.
    pub fn inputs(&self) -> BTreeSet<OutPoint> {
        self.transactions
            .iter()
            .flat_map(|t| t.inputs.iter().copied())
            .collect()
    }

    /// Records a transaction that was handed to the node.
    pub fn record(&mut self, transaction: PendingTransaction) {
        self.transactions.retain(|t| t.txid != transaction.txid);
        self.transactions.push(transaction);
    }

    /// Drops every transaction one of whose inputs the store now records as
    /// spent — by this transaction (it confirmed) or by another (it can no
    /// longer confirm). Returns what was dropped, with whether it confirmed.
    pub fn settle(&mut self, spends: &[SpentOutput]) -> Vec<(PendingTransaction, bool)> {
        let mut settled = Vec::new();
        self.transactions.retain(|t| {
            let spent_by: Vec<Txid> = spends
                .iter()
                .filter(|s| t.inputs.contains(&OutPoint::new(s.txid, s.vout)))
                .map(|s| s.spending_txid)
                .collect();
            if spent_by.is_empty() {
                return true;
            }
            let confirmed = spent_by.iter().all(|by| *by == t.txid);
            settled.push((t.clone(), confirmed));
            false
        });
        settled
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
        match std::fs::read_to_string(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io(e)),
            Ok(text) => serde_json::from_str(&text).map_err(|e| PendingError::Malformed {
                path: path.display().to_string(),
                reason: e.to_string(),
            }),
        }
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
