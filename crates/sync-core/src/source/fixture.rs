//! Deterministic, offline fixture source.
//!
//! A fixture is a JSON file holding consensus-serialized blocks plus the
//! previous outputs of every input. Loading validates structure only:
//!
//! * network name (mainnet refused) and genesis hash;
//! * contiguous heights starting at 1, each block linking to its parent, the
//!   first one to the network's genesis block;
//! * merkle root; BIP141 witness commitment (strictly: a stripped block is
//!   rejected); proof of work against the header's own `bits`, which must be
//!   canonical and within the network's limit; BIP34 height;
//! * exactly one coinbase (first), unique txids, one prevout per input.
//!
//! It does **not** validate scripts, signatures, amounts or that prevouts
//! really exist: fixture transactions are not consensus-valid and prove
//! nothing about a real chain.

use super::{
    block_check::{check_full_block, check_prevouts_within_range, BlockExpectation},
    BlockEvidence, Evidence, ScanBatch, ScanSource, SourceCapabilities, SourceError,
    TxWithPrevouts,
};
use crate::{
    bitcoin::{
        consensus::encode::deserialize_hex, Amount, Block, BlockHash, ScriptBuf, TxOut, Txid,
    },
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, LabNetwork},
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};

/// Format tag of the fixture files this adapter understands.
pub const FIXTURE_FORMAT: &str = "silent-sync-lab/fixture-chain/v1";

const FIXTURE_BACKEND_ID: &str = "fixture";

/// On-disk fixture document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureFile {
    pub format: String,
    pub description: String,
    pub network: LabNetwork,
    pub genesis_hash: BlockHash,
    pub blocks: Vec<FixtureBlock>,
    /// Outputs the fixture author *created for* the documented receiver, as
    /// known from the sending side. Informational for tests; never used by the
    /// scanner.
    #[serde(default)]
    pub expected_detections: Vec<FixtureExpectedDetection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureBlock {
    pub height: u32,
    /// Consensus-serialized block, hex.
    pub block_hex: String,
    /// `prevouts[t][i]` is spent by input `i` of transaction `t`.
    /// The coinbase entry is an empty list.
    pub prevouts: Vec<Vec<FixturePrevout>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturePrevout {
    pub value_sats: u64,
    pub script_pubkey_hex: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureExpectedDetection {
    pub height: u32,
    pub txid: Txid,
    pub vout: u32,
    /// BIP352 output index `k` the sender used for this output within the
    /// transaction. A receiver finds the output for `k` only after it has
    /// found the ones for `0..k`, so an output behind an unknown label hides
    /// every later one of the same transaction.
    pub k: u32,
    pub value_sats: u64,
    pub label: Option<u32>,
    pub note: String,
}

/// A validated, in-memory fixture chain.
#[derive(Debug, Clone)]
pub struct FixtureSource {
    chain: ChainIdentity,
    blocks: Vec<BlockEvidence>,
    expected: Vec<FixtureExpectedDetection>,
}

fn invalid(msg: impl Into<String>) -> SourceError {
    SourceError::InvalidData(msg.into())
}

impl FixtureSource {
    pub fn from_path(path: &Path) -> Result<Self, SourceError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| SourceError::Unavailable(format!("{}: {e}", path.display())))?;
        Self::from_json_str(&text)
    }

    pub fn from_json_str(json: &str) -> Result<Self, SourceError> {
        let file: FixtureFile =
            serde_json::from_str(json).map_err(|e| invalid(format!("fixture JSON: {e}")))?;
        Self::from_file(file)
    }

    pub fn from_file(file: FixtureFile) -> Result<Self, SourceError> {
        if file.format != FIXTURE_FORMAT {
            return Err(invalid(format!(
                "unsupported fixture format `{}` (expected `{FIXTURE_FORMAT}`)",
                file.format
            )));
        }
        let chain = ChainIdentity::verified(file.network, file.genesis_hash)
            .map_err(|e| invalid(e.to_string()))?;
        if file.blocks.is_empty() {
            return Err(invalid("fixture contains no blocks"));
        }

        let mut blocks = Vec::with_capacity(file.blocks.len());
        let mut parent_hash = chain.genesis_hash();
        let mut seen_txids = BTreeSet::<Txid>::new();

        for (index, fb) in file.blocks.iter().enumerate() {
            let expected_height =
                u32::try_from(index + 1).map_err(|_| invalid("fixture has too many blocks"))?;
            if fb.height != expected_height {
                return Err(invalid(format!(
                    "block at position {index} has height {} but {expected_height} was expected \
                     (heights must be contiguous and start at 1)",
                    fb.height
                )));
            }
            let (block_ref, txs) = decode_block(fb, chain.network(), parent_hash, &mut seen_txids)?;
            parent_hash = block_ref.hash;
            blocks.push(BlockEvidence {
                block: block_ref,
                evidence: Evidence::FullBlock(txs),
            });
        }

        check_prevouts_within_range(&blocks).map_err(invalid)?;
        Ok(Self {
            chain,
            blocks,
            expected: file.expected_detections,
        })
    }

    /// Sender-side knowledge recorded in the fixture file (see
    /// [`FixtureFile::expected_detections`]).
    pub fn expected_detections(&self) -> &[FixtureExpectedDetection] {
        &self.expected
    }

    fn first_height(&self) -> u32 {
        1
    }

    fn last_height(&self) -> u32 {
        // Validated in `from_file`: non-empty and the count fits in u32.
        self.blocks.last().map_or(0, |b| b.block.height)
    }
}

fn decode_block(
    fb: &FixtureBlock,
    network: LabNetwork,
    parent_hash: BlockHash,
    seen_txids: &mut BTreeSet<Txid>,
) -> Result<(BlockRef, Vec<TxWithPrevouts>), SourceError> {
    let height = fb.height;
    let at = |e: String| invalid(format!("block {height}: {e}"));
    let block: Block =
        deserialize_hex(&fb.block_hex).map_err(|e| at(format!("cannot decode block_hex: {e}")))?;

    let prevouts = fb
        .prevouts
        .iter()
        .map(|tx_prevouts| {
            tx_prevouts
                .iter()
                .map(|p| {
                    ScriptBuf::from_hex(&p.script_pubkey_hex)
                        .map(|script_pubkey| TxOut {
                            value: Amount::from_sat(p.value_sats),
                            script_pubkey,
                        })
                        .map_err(|e| at(format!("bad prevout script: {e}")))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;

    check_full_block(
        block,
        prevouts,
        BlockExpectation {
            network,
            height,
            parent: Some(parent_hash),
            hash: None,
        },
        seen_txids,
    )
    .map_err(at)
}

impl ScanSource for FixtureSource {
    fn backend_id(&self) -> &str {
        FIXTURE_BACKEND_ID
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            evidence_kind: EvidenceKind::FullBlockWithPrevouts,
            verifies_merkle_root: true,
            verifies_witness_commitment: true,
            verifies_proof_of_work: true,
            consensus_validated: false,
            remote: false,
        }
    }

    fn chain(&self) -> ChainIdentity {
        self.chain
    }

    fn tip(&self) -> Result<BlockRef, SourceError> {
        self.blocks
            .last()
            .map(|b| b.block)
            .ok_or_else(|| invalid("fixture contains no blocks"))
    }

    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        let (first, last) = (self.first_height(), self.last_height());
        if height < first || height > last {
            return Err(SourceError::RangeUnavailable {
                from: height,
                to: height,
                available_from: first,
                available_to: last,
            });
        }
        Ok(self.blocks[(height - 1) as usize].block)
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        let (first, last) = (self.first_height(), self.last_height());
        if from < first || to > last {
            return Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: first,
                available_to: last,
            });
        }
        // Heights are contiguous from 1, so height h lives at index h - 1.
        let blocks = self.blocks[(from - 1) as usize..=(to - 1) as usize].to_vec();
        Ok(ScanBatch { blocks })
    }
}
