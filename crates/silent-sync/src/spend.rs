//! What a wallet needs to spend a detected output (feature `spend`).
//!
//! Spending a silent payment output takes the output's *tweak*: the private
//! key of the output is the spend private key plus that tweak. The tweak is
//! sensitive — on its own it ties the output to the receiver — and nothing
//! in this crate stores it: a store holds no key material.
//!
//! [`recover_spendable`] therefore derives it again when it is needed: it
//! fetches the block that contains the output from a full-block source,
//! scans the output's transaction with the receiver's scanner, and returns
//! the tweak together with the output, in a type that does not print it.
//! The caller uses it to sign and drops it.
//!
//! This crate does not sign and does not send. The spend private key never
//! comes near it.

use crate::{
    bitcoin::{secp256k1::SecretKey, OutPoint, TxOut},
    engine::{check_batch, ScanError},
    models::{BlockRef, DetectedOutput, EvidenceKind},
    protocol::{ProtocolError, ReceiveScanner, TxScanOutcome},
    source::{Evidence, ScanSource, SourceError},
};
use std::fmt;

/// A detected output together with what is needed to spend it.
///
/// Holds the output's tweak, which is **sensitive**: the type is not
/// serializable and its `Debug` output leaves the tweak out. Keep it only as
/// long as it takes to sign.
#[derive(Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpendableOutput {
    /// The output.
    pub outpoint: OutPoint,
    /// Its value and script, as the block has them.
    pub txout: TxOut,
    /// BIP352 label integer, `None` for the unlabelled address.
    pub label: Option<u32>,
    /// Block that contains the output.
    pub block: BlockRef,
    tweak: SecretKey,
}

impl SpendableOutput {
    /// The output's tweak (including the label tweak for a labelled output):
    /// the spend private key plus this is the output's private key.
    ///
    /// **Sensitive.** Do not store, log or display it.
    pub fn sensitive_tweak(&self) -> SecretKey {
        self.tweak
    }
}

impl fmt::Debug for SpendableOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpendableOutput")
            .field("outpoint", &self.outpoint)
            .field("txout", &self.txout)
            .field("label", &self.label)
            .field("block", &self.block)
            .field("tweak", &"<redacted>")
            .finish()
    }
}

/// Why the data to spend an output could not be recovered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpendError {
    #[error(transparent)]
    /// The source could not serve the block consistently.
    Scan(#[from] ScanError),
    #[error(
        "source `{backend_id}` does not supply full blocks; an output can only be prepared for \
         spending from the transactions themselves"
    )]
    /// The source is a tweak index. What it lists is its claim; a key
    /// derived from it would be derived from that claim.
    NotFullBlocks {
        /// Backend that was asked.
        backend_id: String,
    },
    #[error(
        "the output {outpoint} was detected through `{found}` evidence; only outputs detected \
         through full blocks are prepared for spending"
    )]
    /// The detection itself rests on an index's claim.
    DetectionNotFromFullBlocks {
        /// The output.
        outpoint: OutPoint,
        /// The kind of evidence the detection rests on.
        found: &'static str,
    },
    #[error("the detection is on {detected}, the source on {source_network}")]
    /// Detection and source are on different networks.
    NetworkMismatch {
        /// Network of the detection.
        detected: String,
        /// Network of the source.
        source_network: String,
    },
    #[error(
        "the block at height {height} is no longer the block the output was detected in \
         (a reorganization?); sync, then try again"
    )]
    /// The source has another block at the detection's height.
    BlockChanged {
        /// Height of the detection.
        height: u32,
    },
    #[error("the block at height {height} does not contain the transaction of {outpoint}")]
    /// The block does not contain the output's transaction.
    TransactionMissing {
        /// Height of the detection.
        height: u32,
        /// The output.
        outpoint: OutPoint,
    },
    #[error(
        "scanning the transaction again did not yield {outpoint} as it was detected (same \
         output, amount and label)"
    )]
    /// The scan of the transaction does not reproduce the detection.
    NotReproduced {
        /// The output.
        outpoint: OutPoint,
    },
    #[error("the transaction of {outpoint} could not be scanned: {reason}")]
    /// The scanner failed on the transaction.
    Transaction {
        /// The output.
        outpoint: OutPoint,
        /// Why.
        reason: ProtocolError,
    },
}

impl From<SourceError> for SpendError {
    fn from(e: SourceError) -> Self {
        SpendError::Scan(ScanError::Source(e))
    }
}

/// Derives again what is needed to spend `detection`: fetches its block from
/// `source`, scans its transaction with `scanner` and returns the output with
/// its tweak.
///
/// Fails unless everything matches what was detected: the block (same hash),
/// the transaction, the output, its amount and its label. The detection must
/// rest on full blocks, and what the source serves for that height must be a
/// full block (of the kind the source declares).
pub fn recover_spendable(
    source: &dyn ScanSource,
    scanner: &ReceiveScanner,
    detection: &DetectedOutput,
) -> Result<SpendableOutput, SpendError> {
    let outpoint = OutPoint::new(detection.txid, detection.vout);
    if detection.provenance.evidence_kind != EvidenceKind::FullBlockWithPrevouts {
        return Err(SpendError::DetectionNotFromFullBlocks {
            outpoint,
            found: match detection.provenance.evidence_kind {
                EvidenceKind::TweakIndex => "tweak_index",
                _ => "unknown",
            },
        });
    }
    if source.chain().network() != detection.network {
        return Err(SpendError::NetworkMismatch {
            detected: detection.network.to_string(),
            source_network: source.chain().network().to_string(),
        });
    }
    let height = detection.block.height;
    let batch = source.fetch_range(height, height)?;
    check_batch(
        source.backend_id(),
        &batch.blocks,
        height,
        height,
        source.capabilities().evidence_kind,
    )?;
    let block = batch
        .blocks
        .first()
        .ok_or(SpendError::BlockChanged { height })?;
    if block.block != detection.block {
        return Err(SpendError::BlockChanged { height });
    }
    let Evidence::FullBlock(txs) = &block.evidence else {
        return Err(SpendError::NotFullBlocks {
            backend_id: source.backend_id().to_string(),
        });
    };
    let entry = txs
        .iter()
        .find(|entry| entry.tx.compute_txid() == detection.txid)
        .ok_or(SpendError::TransactionMissing { height, outpoint })?;
    let matches = match scanner
        .scan_tx(&entry.tx, &entry.prevouts)
        .map_err(|reason| SpendError::Transaction { outpoint, reason })?
    {
        TxScanOutcome::Scanned { matches } => matches,
        TxScanOutcome::NotEligible(_) => Vec::new(),
    };
    let found = matches
        .iter()
        .find(|m| m.outpoint == outpoint)
        .filter(|m| m.amount.to_sat() == detection.value_sats && m.label == detection.label)
        .ok_or(SpendError::NotReproduced { outpoint })?;
    Ok(SpendableOutput {
        outpoint,
        txout: TxOut {
            value: found.amount,
            script_pubkey: found.script_pubkey.clone(),
        },
        label: found.label,
        block: detection.block,
        tweak: found.tweak_for_spending(),
    })
}
