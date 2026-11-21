//! Auditing a store against a reference source.
//!
//! [`cross_check`](crate::verify::cross_check) compares what two sources
//! answer *now*. It cannot find a source that lied only while the store was
//! being filled (an index that omitted a payment once, or invented one).
//! [`audit_store`] closes that gap: it scans the stored range again through a
//! *reference* source and compares the result with what the store holds —
//! block by block, output by output. The store is only read.
//!
//! What agreement proves and what it does not: if the audit finds no
//! difference, the store holds exactly what the reference shows for this
//! receiver in the audited range. That is worth as much as the reference:
//! it means something when the reference is a full-block source backed by
//! the user's own node, and little when it is another server the user does
//! not control. It says nothing about blocks outside the range, and nothing
//! about whether a detected output has been spent since.

use crate::{
    bitcoin::{OutPoint, Txid},
    engine::{check_batch, scan_blocks, ScanCounters, ScanError, SCAN_CHUNK_BLOCKS},
    models::{BlockRef, DetectedOutput, EvidenceKind},
    network::ChainIdentity,
    protocol::ReceiveScanner,
    source::{ScanSource, SourceError},
    store::{StoreError, SyncStore},
    sync::binding_difference,
    verify::{explain_candidate_only, Difference, Explanation, Side},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Which part of a store [`audit_store`] looks at.
///
/// ```
/// use silent_sync::audit::AuditOptions;
/// let options = AuditOptions::default().with_first_height(Some(500));
/// assert_eq!(options.first_height, Some(500));
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct AuditOptions {
    /// First height to audit (`None`: the store's birthday).
    pub first_height: Option<u32>,
    /// Last height to audit (`None`: the store's checkpoint).
    pub last_height: Option<u32>,
}

impl AuditOptions {
    /// First height to audit (`None`: the store's birthday).
    pub fn with_first_height(mut self, first_height: Option<u32>) -> Self {
        self.first_height = first_height;
        self
    }

    /// Last height to audit (`None`: the store's checkpoint).
    pub fn with_last_height(mut self, last_height: Option<u32>) -> Self {
        self.last_height = last_height;
        self
    }
}

/// Why an audit could not be carried out. A *difference* between the store
/// and the reference is not an error: it is reported in [`StoreAuditReport`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuditError {
    #[error(transparent)]
    /// The store failed, or what it holds breaks a rule every store enforces.
    Store(#[from] StoreError),
    #[error(transparent)]
    /// The reference could not serve the range consistently.
    Scan(#[from] ScanError),
    #[error("the store is bound to a different {field}: stored `{stored}`, given `{given}`")]
    /// The store is for another chain, receiver, label set or scanner
    /// version than the one the audit was given. (Another *backend* is fine:
    /// that is what an audit is for.)
    BindingMismatch {
        /// Which part of the binding differs.
        field: &'static str,
        /// What the store is bound to.
        stored: String,
        /// What the call was given.
        given: String,
    },
    #[error("the store holds no scanned blocks: there is nothing to audit")]
    /// Nothing has been synced into the store yet.
    NothingStored,
    #[error(
        "heights {first}..={last} are not a range inside the stored blocks \
         {first_stored}..={last_stored}"
    )]
    /// The requested range is empty or reaches outside what is stored.
    RangeOutsideStore {
        /// First height asked for.
        first: u32,
        /// Last height asked for.
        last: u32,
        /// First stored height (the birthday).
        first_stored: u32,
        /// Last stored height (the checkpoint).
        last_stored: u32,
    },
}

impl From<SourceError> for AuditError {
    fn from(e: SourceError) -> Self {
        AuditError::Scan(ScanError::Source(e))
    }
}

/// Result of [`audit_store`]. Contains no key material.
///
/// In [`Difference`], the *candidate* side is the store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct StoreAuditReport {
    /// Chain the store is for.
    pub chain: ChainIdentity,
    /// Backend the store was filled from.
    pub store_backend: String,
    /// Backend the store was audited against.
    pub reference_backend: String,
    /// Kind of evidence the reference supplied. Only a full-block reference
    /// derives its result from the transactions themselves.
    pub reference_evidence: EvidenceKind,
    /// First height audited.
    pub first_height: u32,
    /// Last height audited.
    pub last_height: u32,
    /// Labels that were scanned for.
    pub labels_scanned: Vec<u32>,
    /// Number of stored blocks compared with the reference.
    pub blocks_compared: u64,
    /// Outputs the store holds exactly as the reference detects them.
    pub agreed: u64,
    /// Outputs the store holds that the reference does not detect.
    pub stored_only: u64,
    /// Outputs the reference detects that the store does not hold.
    pub reference_only: u64,
    /// Every disagreement, ascending by height.
    pub differences: Vec<Difference>,
    /// Heights at which the stored block is not the reference's block (the
    /// store is on another chain there: sync it, then audit again).
    pub block_hash_mismatches: Vec<u32>,
    /// No differences at all.
    pub identical: bool,
    /// Differences without a specific explanation.
    pub unexplained: u64,
}

fn key(d: &DetectedOutput) -> OutPoint {
    OutPoint::new(d.txid, d.vout)
}

/// Scans the stored range again through `reference` and compares the result
/// with what `store` holds. The store is not changed.
///
/// The store must be bound to the same chain, receiver, label set and scanner
/// version as the call; it may have been filled from any backend. Errors only
/// when the audit cannot be carried out; differences are in the report.
pub fn audit_store(
    reference: &dyn ScanSource,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    store: &dyn SyncStore,
    options: &AuditOptions,
) -> Result<StoreAuditReport, AuditError> {
    let meta = store.meta()?.ok_or(StoreError::NotInitialized)?;
    if let Some(d) = binding_difference(&meta, reference.chain(), None, scanner, wallet_id) {
        return Err(AuditError::BindingMismatch {
            field: d.field,
            stored: d.stored,
            given: d.given,
        });
    }
    let checkpoint = store.checkpoint()?.ok_or(AuditError::NothingStored)?;
    let first = options.first_height.unwrap_or(meta.birthday);
    let last = options.last_height.unwrap_or(checkpoint.height);
    if first > last || first < meta.birthday || last > checkpoint.height {
        return Err(AuditError::RangeOutsideStore {
            first,
            last,
            first_stored: meta.birthday,
            last_stored: checkpoint.height,
        });
    }
    let reference_id = reference.backend_id().to_string();

    // What the store holds in the range, by height.
    let mut stored_by_height: BTreeMap<u32, Vec<DetectedOutput>> = BTreeMap::new();
    for d in store.detections()? {
        if (first..=last).contains(&d.block.height) {
            stored_by_height.entry(d.block.height).or_default().push(d);
        }
    }

    let mut report = StoreAuditReport {
        chain: meta.chain,
        store_backend: meta.backend_id.clone(),
        reference_backend: reference_id.clone(),
        reference_evidence: reference.capabilities().evidence_kind,
        first_height: first,
        last_height: last,
        labels_scanned: scanner.labels().collect(),
        blocks_compared: 0,
        agreed: 0,
        stored_only: 0,
        reference_only: 0,
        differences: Vec::new(),
        block_hash_mismatches: Vec::new(),
        identical: false,
        unexplained: 0,
    };

    // The reference is held to the rules of a plain scan of the range:
    // complete, ordered batches, chunks that link to each other, and no
    // transaction listed twice anywhere in the range.
    let mut seen = BTreeSet::<Txid>::new();
    let mut previous: Option<BlockRef> = None;
    let mut next = first;
    loop {
        let chunk_last = last.min(next.saturating_add(SCAN_CHUNK_BLOCKS - 1));
        let batch = reference.fetch_range(next, chunk_last)?;
        check_batch(&reference_id, &batch.blocks, next, chunk_last)?;
        if let (Some(prev), Some(first_block)) = (previous, batch.blocks.first()) {
            if first_block.block.prev_hash != prev.hash {
                return Err(AuditError::Scan(ScanError::InconsistentBatch {
                    backend_id: reference_id,
                    reason: format!(
                        "block {} does not link to block {} (the chain changed during the audit?)",
                        first_block.block.height, prev.height
                    ),
                }));
            }
        }
        previous = batch.blocks.last().map(|b| b.block);

        for r_block in &batch.blocks {
            let height = r_block.block.height;
            let stored_block = store.block_at(height)?.ok_or_else(|| {
                StoreError::Invariant(format!(
                    "no stored block at height {height} below the checkpoint"
                ))
            })?;
            let stored = stored_by_height.remove(&height).unwrap_or_default();
            // A detection must belong to the block the store has at its
            // height; anything else is a broken store, not a difference.
            if let Some(d) = stored.iter().find(|d| d.block != stored_block) {
                return Err(AuditError::Store(StoreError::Invariant(format!(
                    "the stored detection {}:{} at height {height} belongs to block {}, \
                     but the stored block at that height is {}",
                    d.txid, d.vout, d.block.hash, stored_block.hash
                ))));
            }
            let same_block = stored_block.hash == r_block.block.hash;
            if !same_block {
                report.block_hash_mismatches.push(height);
            }
            report.blocks_compared += 1;

            let found = scan_blocks(
                &reference_id,
                meta.chain,
                scanner,
                std::slice::from_ref(r_block),
                &mut ScanCounters::default(),
                &mut seen,
            )?;
            let by_s: BTreeMap<OutPoint, &DetectedOutput> =
                stored.iter().map(|d| (key(d), d)).collect();
            let by_r: BTreeMap<OutPoint, &DetectedOutput> =
                found.iter().map(|d| (key(d), d)).collect();
            let all: BTreeSet<OutPoint> = by_s.keys().chain(by_r.keys()).copied().collect();

            for outpoint in all {
                let (s, r) = (by_s.get(&outpoint).copied(), by_r.get(&outpoint).copied());
                let (only_in, explanation) = match (s, r) {
                    (Some(s), Some(r)) => {
                        if !same_block {
                            (None, Explanation::BlockHashDiffers)
                        } else if s.value_sats != r.value_sats || s.label != r.label {
                            (None, Explanation::DetailsDiffer)
                        } else {
                            report.agreed += 1;
                            continue;
                        }
                    }
                    (Some(s), None) => {
                        report.stored_only += 1;
                        let why = if same_block {
                            explain_candidate_only(scanner, r_block, s)
                        } else {
                            Explanation::BlockHashDiffers
                        };
                        (Some(Side::Candidate), why)
                    }
                    (None, Some(_)) => {
                        report.reference_only += 1;
                        let why = if same_block {
                            Explanation::StoreOmittedOutput
                        } else {
                            Explanation::BlockHashDiffers
                        };
                        (Some(Side::Reference), why)
                    }
                    (None, None) => continue,
                };
                if matches!(explanation, Explanation::Unexplained { .. }) {
                    report.unexplained += 1;
                }
                report.differences.push(Difference {
                    height,
                    txid: outpoint.txid,
                    vout: outpoint.vout,
                    only_in,
                    candidate: s.cloned(),
                    reference: r.cloned(),
                    explanation,
                });
            }
        }
        if chunk_last == last {
            break;
        }
        next = chunk_last + 1;
    }

    report.identical = report.differences.is_empty() && report.block_hash_mismatches.is_empty();
    Ok(report)
}
