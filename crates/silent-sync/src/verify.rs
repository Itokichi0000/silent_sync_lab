//! Cross-checking one backend against another.
//!
//! [`cross_check`] scans the same range through a *candidate* source and a
//! *reference* source with the same receiver, and reports every output that
//! only one of them detected — together with the most specific explanation
//! the two sets of evidence allow. The reference is expected to be a
//! full-block source the user trusts (their own node): only full blocks carry
//! enough information to say *why* an index-based result differs.
//!
//! What agreement proves and what it does not (design spec §5.6): if both
//! sources agree, the candidate served everything the reference shows for
//! this receiver in this range. It does not prove the reference is complete
//! or honest, and agreement between two servers one does not control proves
//! little. This function is a diagnostic for the lab, not a trust anchor.

use crate::{
    bitcoin::{OutPoint, Txid},
    engine::{check_batch, scan_blocks, ScanCounters, ScanError, SCAN_CHUNK_BLOCKS},
    models::{BlockRef, DetectedOutput},
    network::ChainIdentity,
    protocol::{ReceiveScanner, TxScanOutcome},
    source::{BlockEvidence, Evidence, ScanSource},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Which side detected an output the other did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// What is being checked: the candidate source of [`cross_check`], or
    /// the store of [`audit_store`](crate::audit::audit_store).
    Candidate,
    /// The source it is checked against.
    Reference,
}

/// Why the two sources disagree about one output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Explanation {
    /// The two sources do not even agree on which block is at this height.
    BlockHashDiffers,
    /// Candidate-only: BIP352 says the transaction must not be scanned (the
    /// reference, which sees the inputs, applied the rule; an index did not).
    ReferenceNotEligible {
        /// The BIP352 reason the transaction is not eligible.
        reason: &'static str,
    },
    /// Candidate-only: the reference block contains no such transaction.
    ReferenceTransactionMissing,
    /// Candidate-only: the reference has the transaction and scanned it, and
    /// this output is not among its matches — the candidate used other tweak
    /// data, or lists an output the transaction does not have.
    ReferenceScannedNoMatch,
    /// Reference-only: the candidate's evidence has no entry for the
    /// transaction (an index omitted it).
    CandidateOmittedTransaction,
    /// Reference-only: the candidate lists the transaction with tweak data
    /// that differs from what its inputs give.
    CandidateTweakDiffers,
    /// Reference-only: the candidate lists the transaction but not this output.
    CandidateOmittedOutput,
    /// Reference-only: the candidate lists this output with another key or value.
    CandidateOutputDiffers,
    /// Reference-only, when a store is audited: the store has no record of
    /// this output although it holds the block. The evidence the store was
    /// filled from is not kept, so the cause cannot be narrowed down further
    /// (the store's source omitted the payment, or the store was altered).
    StoreOmittedOutput,
    /// Both detected the output but disagree on value or label.
    DetailsDiffer,
    /// None of the above could be established from the evidence at hand.
    Unexplained {
        /// What was looked at without finding the cause.
        note: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
/// One output about which the two sources disagree.
pub struct Difference {
    /// Height of the block.
    pub height: u32,
    /// Transaction of the output.
    pub txid: Txid,
    /// Index of the output.
    pub vout: u32,
    /// The side that detected it alone; `None` when both did, differently.
    pub only_in: Option<Side>,
    /// What the candidate detected (for an audit: what the store holds).
    pub candidate: Option<DetectedOutput>,
    /// What the reference detected.
    pub reference: Option<DetectedOutput>,
    /// The most specific cause the evidence allows.
    pub explanation: Explanation,
}

/// Result of [`cross_check`]. Contains no key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct VerificationReport {
    /// Chain that was compared.
    pub chain: ChainIdentity,
    /// Backend that was checked.
    pub candidate_backend: String,
    /// Backend it was checked against.
    pub reference_backend: String,
    /// First height compared.
    pub first_height: u32,
    /// Last height compared.
    pub last_height: u32,
    /// Labels that were scanned for.
    pub labels_scanned: Vec<u32>,
    /// Outputs both sources detected identically.
    pub agreed: u64,
    /// Outputs only the candidate detected.
    pub candidate_only: u64,
    /// Outputs only the reference detected.
    pub reference_only: u64,
    /// Every disagreement, ascending by height.
    pub differences: Vec<Difference>,
    /// Heights at which the two sources report different block hashes.
    pub block_hash_mismatches: Vec<u32>,
    /// No differences at all.
    pub identical: bool,
    /// Differences without a specific explanation.
    pub unexplained: u64,
}

fn key(d: &DetectedOutput) -> OutPoint {
    OutPoint::new(d.txid, d.vout)
}

/// Explains an output only the candidate detected, from the reference block.
pub(crate) fn explain_candidate_only(
    scanner: &ReceiveScanner,
    reference: &BlockEvidence,
    d: &DetectedOutput,
) -> Explanation {
    let Evidence::FullBlock(txs) = &reference.evidence else {
        return Explanation::Unexplained {
            note: "the reference is not a full-block source".to_string(),
        };
    };
    let Some(entry) = txs.iter().find(|t| t.tx.compute_txid() == d.txid) else {
        return Explanation::ReferenceTransactionMissing;
    };
    match scanner.scan_tx(&entry.tx, &entry.prevouts) {
        Ok(TxScanOutcome::NotEligible(reason)) => Explanation::ReferenceNotEligible {
            reason: reason.as_str(),
        },
        Ok(TxScanOutcome::Scanned { .. }) => Explanation::ReferenceScannedNoMatch,
        Err(e) => Explanation::Unexplained {
            note: format!("the reference could not scan the transaction: {e}"),
        },
    }
}

/// Explains an output only the reference detected, from the candidate block.
fn explain_reference_only(
    candidate: &BlockEvidence,
    reference: &BlockEvidence,
    d: &DetectedOutput,
) -> Explanation {
    let reference_tx = match &reference.evidence {
        Evidence::FullBlock(txs) => txs.iter().find(|t| t.tx.compute_txid() == d.txid),
        Evidence::TweakIndex(_) => None,
    };
    match &candidate.evidence {
        Evidence::FullBlock(txs) => {
            if txs.iter().any(|t| t.tx.compute_txid() == d.txid) {
                Explanation::Unexplained {
                    note: "the candidate has the transaction but did not match the output"
                        .to_string(),
                }
            } else {
                Explanation::CandidateOmittedTransaction
            }
        }
        Evidence::TweakIndex(indexed) => {
            let Some(listed) = indexed.iter().find(|t| t.txid == d.txid) else {
                return Explanation::CandidateOmittedTransaction;
            };
            if let Some(entry) = reference_tx {
                match ReceiveScanner::public_tweak_data(&entry.tx, &entry.prevouts) {
                    Ok(Ok(tweak)) if tweak != listed.tweak => {
                        return Explanation::CandidateTweakDiffers
                    }
                    _ => {}
                }
                let Some(output) = listed.outputs.iter().find(|o| o.vout == d.vout) else {
                    return Explanation::CandidateOmittedOutput;
                };
                let real = entry.tx.output.get(d.vout as usize);
                let same = real.is_some_and(|r| {
                    r.value.to_sat() == output.value_sats
                        && r.script_pubkey.as_bytes().get(2..) == Some(&output.output_key[..])
                });
                if !same {
                    return Explanation::CandidateOutputDiffers;
                }
            } else if !listed.outputs.iter().any(|o| o.vout == d.vout) {
                return Explanation::CandidateOmittedOutput;
            }
            Explanation::Unexplained {
                note: "the candidate lists the transaction and the output".to_string(),
            }
        }
    }
}

/// Scans `from..=to` through both sources and compares the detections.
///
/// Errors only when a source cannot serve the range consistently; a
/// *difference in results* is reported in the return value, not as an error.
pub fn cross_check(
    candidate: &dyn ScanSource,
    reference: &dyn ScanSource,
    scanner: &ReceiveScanner,
    expected_chain: ChainIdentity,
    from: u32,
    to: u32,
) -> Result<VerificationReport, ScanError> {
    for source in [candidate, reference] {
        if source.chain() != expected_chain {
            return Err(ScanError::ChainMismatch {
                backend_id: source.backend_id().to_string(),
                expected: expected_chain,
                actual: source.chain(),
            });
        }
    }
    if from > to {
        return Err(ScanError::InvalidRange { from, to });
    }
    let (candidate_id, reference_id) = (
        candidate.backend_id().to_string(),
        reference.backend_id().to_string(),
    );

    let mut report = VerificationReport {
        chain: expected_chain,
        candidate_backend: candidate_id.clone(),
        reference_backend: reference_id.clone(),
        first_height: from,
        last_height: to,
        labels_scanned: scanner.labels().collect(),
        agreed: 0,
        candidate_only: 0,
        reference_only: 0,
        differences: Vec::new(),
        block_hash_mismatches: Vec::new(),
        identical: false,
        unexplained: 0,
    };

    // Each side is held to the rules of a plain scan of the range: complete,
    // ordered batches, chunks that link to each other, and no transaction
    // listed twice anywhere in the range.
    let (mut seen_c, mut seen_r) = (BTreeSet::new(), BTreeSet::new());
    let scan_one = |backend_id: &str, block: &BlockEvidence, seen: &mut BTreeSet<Txid>| {
        scan_blocks(
            backend_id,
            expected_chain,
            scanner,
            std::slice::from_ref(block),
            &mut ScanCounters::default(),
            seen,
        )
    };
    let linked = |backend_id: &str, previous: Option<BlockRef>, blocks: &[BlockEvidence]| match (
        previous,
        blocks.first(),
    ) {
        (Some(prev), Some(first)) if first.block.prev_hash != prev.hash => {
            Err(ScanError::InconsistentBatch {
                backend_id: backend_id.to_string(),
                reason: format!(
                    "block {} does not link to block {} (the chain changed during the scan?)",
                    first.block.height, prev.height
                ),
            })
        }
        _ => Ok(blocks.last().map(|b| b.block)),
    };
    let (mut previous_c, mut previous_r) = (None, None);

    let mut next = from;
    loop {
        let last = to.min(next.saturating_add(SCAN_CHUNK_BLOCKS - 1));
        let candidate_batch = candidate.fetch_range(next, last)?;
        check_batch(
            &candidate_id,
            &candidate_batch.blocks,
            next,
            last,
            candidate.capabilities().evidence_kind,
        )?;
        previous_c = linked(&candidate_id, previous_c, &candidate_batch.blocks)?;
        let reference_batch = reference.fetch_range(next, last)?;
        check_batch(
            &reference_id,
            &reference_batch.blocks,
            next,
            last,
            reference.capabilities().evidence_kind,
        )?;
        previous_r = linked(&reference_id, previous_r, &reference_batch.blocks)?;

        for (c_block, r_block) in candidate_batch.blocks.iter().zip(&reference_batch.blocks) {
            let height = r_block.block.height;
            let same_block: bool = same_hash(c_block.block, r_block.block);
            if !same_block {
                report.block_hash_mismatches.push(height);
            }
            let found_c = scan_one(&candidate_id, c_block, &mut seen_c)?;
            let found_r = scan_one(&reference_id, r_block, &mut seen_r)?;
            let by_c: BTreeMap<OutPoint, &DetectedOutput> =
                found_c.iter().map(|d| (key(d), d)).collect();
            let by_r: BTreeMap<OutPoint, &DetectedOutput> =
                found_r.iter().map(|d| (key(d), d)).collect();
            let all: BTreeSet<OutPoint> = by_c.keys().chain(by_r.keys()).copied().collect();

            for outpoint in all {
                let (c, r) = (by_c.get(&outpoint).copied(), by_r.get(&outpoint).copied());
                let (only_in, explanation) = match (c, r) {
                    (Some(c), Some(r)) => {
                        if !same_block {
                            (None, Explanation::BlockHashDiffers)
                        } else if c.value_sats != r.value_sats || c.label != r.label {
                            (None, Explanation::DetailsDiffer)
                        } else {
                            report.agreed += 1;
                            continue;
                        }
                    }
                    (Some(c), None) => {
                        report.candidate_only += 1;
                        let why = if same_block {
                            explain_candidate_only(scanner, r_block, c)
                        } else {
                            Explanation::BlockHashDiffers
                        };
                        (Some(Side::Candidate), why)
                    }
                    (None, Some(r)) => {
                        report.reference_only += 1;
                        let why = if same_block {
                            explain_reference_only(c_block, r_block, r)
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
                    candidate: c.cloned(),
                    reference: r.cloned(),
                    explanation,
                });
            }
        }
        if last == to {
            break;
        }
        next = last + 1;
    }

    report.identical = report.differences.is_empty() && report.block_hash_mismatches.is_empty();
    Ok(report)
}

fn same_hash(a: BlockRef, b: BlockRef) -> bool {
    a.height == b.height && a.hash == b.hash
}
