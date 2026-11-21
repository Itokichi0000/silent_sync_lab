//! Bitcoin Core backend over the node's REST interface.
//!
//! Requires a node the user runs themselves, started with `-rest=1`, version
//! 30 or later (`/rest/spenttxouts`). Everything needed for BIP352 scanning is
//! read in binary form: the block with witnesses, and the outputs it spends.
//!
//! * No credentials: the REST interface is unauthenticated and read-only, so
//!   this adapter never sees an RPC cookie or password. The flip side is that
//!   anything on the same host can read the node's REST data too.
//! * Loopback only: the URL must be `http://127.x.y.z:<port>` (see
//!   `source::http`). Nothing here talks to a third party.
//! * Anchored to the network's genesis block: every block that is used was
//!   reached by following headers from the lab network's well-known genesis
//!   hash, checking each link and each header's proof of work. A responder
//!   that merely *says* "regtest" while serving another chain's blocks
//!   (mainnet in particular) cannot produce such a chain.
//! * Every response is re-checked: requested hash, merkle root, strict
//!   witness commitment, height, prevout counts, trailing bytes — so a block
//!   fetched without witnesses or a mismatched response is an error.
//!
//! What is *not* verified: that the spent outputs returned by the node are
//! the real ones (nothing in a block commits to them, apart from outputs
//! created inside the fetched range, which are cross-checked), and that the
//! chain is the most-work chain. Both are the node's word.
//!
//! Scan private keys never leave the process: requests contain heights and
//! block hashes only.

use super::{
    block_check::{
        check_full_block, check_prevouts_within_range, check_proof_of_work, BlockExpectation,
    },
    http::LoopbackHttp,
    BlockEvidence, Evidence, ScanBatch, ScanSource, SourceCapabilities, SourceError, SourceStats,
};
use crate::{
    bitcoin::{
        block::Header,
        consensus::{encode::VarInt, Decodable},
        hashes::Hash,
        Block, BlockHash, TxOut,
    },
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, LabNetwork, NetworkError},
};
use std::{collections::BTreeSet, sync::Mutex};

pub const CORE_REST_BACKEND_ID: &str = "bitcoin_core_rest";

/// Upper bound on transactions per block accepted from `spenttxouts`.
const MAX_TXS_PER_BLOCK: u64 = 1_000_000;
/// Headers requested per call (Bitcoin Core's own maximum).
const HEADERS_PER_REQUEST: usize = 2000;

/// A Bitcoin Core node reached through its REST interface.
#[derive(Debug)]
pub struct CoreRestSource {
    http: LoopbackHttp,
    chain: ChainIdentity,
    pruned: bool,
    /// `anchored[h]` is the hash at height `h` of the header chain verified
    /// link by link from the network's genesis block.
    anchored: Mutex<Vec<BlockHash>>,
}

#[derive(serde::Deserialize)]
struct ChainInfo {
    chain: String,
    blocks: u32,
    bestblockhash: BlockHash,
    #[serde(default)]
    pruned: bool,
}

impl CoreRestSource {
    /// Connects to `rest_url` (e.g. `http://127.0.0.1:18443`), refusing
    /// non-loopback hosts, non-HTTP schemes and mainnet nodes.
    pub fn connect(rest_url: &str) -> Result<Self, SourceError> {
        let http = LoopbackHttp::new(rest_url, "REST")?;
        let info = chain_info(&http)?;
        let network = network_from_core_chain(&info.chain)?;
        // The anchor is the genesis hash *this crate* knows for the network,
        // never one reported by the node.
        let chain = ChainIdentity::for_network(network);
        let source = Self {
            http,
            chain,
            pruned: info.pruned,
            anchored: Mutex::new(vec![chain.genesis_hash()]),
        };
        // Fails with a clear message if the node's block 0 is another chain's.
        let genesis = source.block_hash_at(0)?;
        ChainIdentity::verified(network, genesis)?;
        Ok(source)
    }

    /// Whether the node reported itself as pruned when connecting. A pruned
    /// node cannot serve blocks below its prune height; such requests fail.
    pub fn is_pruned(&self) -> bool {
        self.pruned
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, SourceError> {
        self.http.get(path)
    }

    fn block_hash_at(&self, height: u32) -> Result<BlockHash, SourceError> {
        let body = self.get(&format!("/rest/blockhashbyheight/{height}.hex"))?;
        std::str::from_utf8(&body)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or_else(|| {
                SourceError::InvalidData(format!("blockhashbyheight/{height}: not a block hash"))
            })
    }

    /// Headers on the node's active chain starting at `from` (inclusive).
    fn headers_from(&self, from: BlockHash) -> Result<Vec<Header>, SourceError> {
        let body = self.get(&format!(
            "/rest/headers/{from}.bin?count={HEADERS_PER_REQUEST}"
        ))?;
        if body.len() % Header::SIZE != 0 {
            return Err(SourceError::InvalidData(format!(
                "headers from {from}: {} bytes is not a whole number of headers",
                body.len()
            )));
        }
        body.chunks(Header::SIZE)
            .map(|chunk| {
                let mut cursor = chunk;
                Header::consensus_decode(&mut cursor)
                    .map_err(|e| SourceError::InvalidData(format!("headers from {from}: {e}")))
            })
            .collect()
    }

    /// Makes sure the verified header chain reaches `height` on the node's
    /// *current* chain and returns a copy of it up to that height.
    ///
    /// Stale entries (after a reorg on the node) are dropped first; new ones
    /// are added only by following `prev_blockhash` links from an already
    /// verified hash, with proof of work checked for every header.
    fn anchor_up_to(&self, height: u32) -> Result<Vec<BlockHash>, SourceError> {
        let mut anchored = self
            .anchored
            .lock()
            .map_err(|_| SourceError::Unavailable("internal lock poisoned".to_string()))?;
        let network = self.chain.network();

        // Drop whatever is no longer on the node's active chain.
        while anchored.len() > 1 {
            let top = (anchored.len() - 1) as u32;
            let still_active = match self.block_hash_at(top) {
                Ok(hash) => Some(&hash) == anchored.last(),
                // The node's chain is shorter than our cache.
                Err(SourceError::Unavailable(_)) => false,
                Err(e) => return Err(e),
            };
            if still_active {
                break;
            }
            anchored.pop();
        }

        while anchored.len() <= height as usize {
            let top_height = anchored.len() - 1;
            let top = *anchored
                .last()
                .ok_or_else(|| SourceError::InvalidData("empty header anchor".to_string()))?;
            let headers = self.headers_from(top)?;
            let invalid = |why: String| {
                SourceError::InvalidData(format!(
                    "header chain above height {top_height} ({top}): {why}"
                ))
            };
            match headers.first() {
                Some(first) if first.block_hash() == top => {}
                Some(_) => {
                    return Err(invalid(
                        "response does not start at the requested header".into(),
                    ))
                }
                None => {
                    return Err(SourceError::Unavailable(format!(
                        "block {top} at height {top_height} is not on the node's active chain \
                         (reorganization in progress?); retry"
                    )))
                }
            }
            if headers.len() == 1 {
                return Err(SourceError::RangeUnavailable {
                    from: height,
                    to: height,
                    available_from: 1,
                    available_to: top_height as u32,
                });
            }
            let mut parent = top;
            for (offset, header) in headers.iter().enumerate().skip(1) {
                let at = top_height + offset;
                if header.prev_blockhash != parent {
                    return Err(invalid(format!(
                        "header at height {at} does not link to its parent {parent}"
                    )));
                }
                let hash = check_proof_of_work(header, network)
                    .map_err(|e| invalid(format!("header at height {at}: proof of work: {e}")))?;
                anchored.push(hash);
                parent = hash;
            }
        }
        Ok(anchored[..=height as usize].to_vec())
    }
}

fn chain_info(http: &LoopbackHttp) -> Result<ChainInfo, SourceError> {
    let body = http.get("/rest/chaininfo.json")?;
    serde_json::from_slice(&body).map_err(|e| SourceError::InvalidData(format!("chaininfo: {e}")))
}

/// Maps Bitcoin Core's chain names. `main` is refused explicitly.
fn network_from_core_chain(chain: &str) -> Result<LabNetwork, SourceError> {
    match chain {
        "regtest" => Ok(LabNetwork::Regtest),
        "signet" => Ok(LabNetwork::Signet),
        "main" => Err(NetworkError::MainnetRefused.into()),
        other => Err(NetworkError::Unsupported(super::http::printable(other)).into()),
    }
}

/// Decodes the body of `/rest/spenttxouts/<hash>.bin`: for every transaction
/// of the block, in block order, the outputs its inputs spend (empty for the
/// coinbase).
fn decode_spent_outputs(bytes: &[u8]) -> Result<Vec<Vec<TxOut>>, String> {
    let mut cursor: &[u8] = bytes;
    let count = VarInt::consensus_decode(&mut cursor)
        .map_err(|e| e.to_string())?
        .0;
    if count > MAX_TXS_PER_BLOCK {
        return Err(format!("implausible transaction count {count}"));
    }
    let mut all = Vec::new();
    for _ in 0..count {
        all.push(Vec::<TxOut>::consensus_decode(&mut cursor).map_err(|e| e.to_string())?);
    }
    if !cursor.is_empty() {
        return Err(format!("{} trailing bytes", cursor.len()));
    }
    Ok(all)
}

fn block_ref_from(anchored: &[BlockHash], height: u32) -> BlockRef {
    let h = height as usize;
    BlockRef {
        height,
        hash: anchored[h],
        // The genesis block's parent is the all-zero hash.
        prev_hash: if h == 0 {
            BlockHash::all_zeros()
        } else {
            anchored[h - 1]
        },
    }
}

impl ScanSource for CoreRestSource {
    fn backend_id(&self) -> &str {
        CORE_REST_BACKEND_ID
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            evidence_kind: EvidenceKind::FullBlockWithPrevouts,
            verifies_merkle_root: true,
            verifies_witness_commitment: true,
            verifies_proof_of_work: true,
            // The node's claim: this adapter cannot check it.
            consensus_validated: true,
            remote: false,
        }
    }

    fn chain(&self) -> ChainIdentity {
        self.chain
    }

    fn tip(&self) -> Result<BlockRef, SourceError> {
        let info = chain_info(&self.http)?;
        if network_from_core_chain(&info.chain)? != self.chain.network() {
            return Err(SourceError::InvalidData(
                "the node switched to another chain".to_string(),
            ));
        }
        let anchored = self.anchor_up_to(info.blocks)?;
        let tip = block_ref_from(&anchored, info.blocks);
        if tip.hash != info.bestblockhash {
            return Err(SourceError::Unavailable(format!(
                "the node reports best block {} at height {}, but the verified header chain has {} \
                 there (tip moved during the query?); retry",
                info.bestblockhash, info.blocks, tip.hash
            )));
        }
        Ok(tip)
    }

    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        let anchored = self.anchor_up_to(height)?;
        Ok(block_ref_from(&anchored, height))
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        let tip = chain_info(&self.http)?.blocks;
        // The genesis block carries no BIP34 height and pays nobody.
        if from < 1 || to > tip {
            return Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: 1,
                available_to: tip,
            });
        }
        let anchored = self.anchor_up_to(to)?;

        let mut blocks = Vec::new();
        let mut seen_txids = BTreeSet::new();
        for height in from..=to {
            let at = |e: String| SourceError::InvalidData(format!("block {height}: {e}"));
            let expected = block_ref_from(&anchored, height);
            let hash = expected.hash;
            let body = self.get(&format!("/rest/block/{hash}.bin"))?;
            let mut cursor: &[u8] = &body;
            let block = Block::consensus_decode(&mut cursor)
                .map_err(|e| at(format!("cannot decode block: {e}")))?;
            if !cursor.is_empty() {
                return Err(at(format!("{} trailing bytes after block", cursor.len())));
            }
            let spent = self.get(&format!("/rest/spenttxouts/{hash}.bin"))?;
            let prevouts =
                decode_spent_outputs(&spent).map_err(|e| at(format!("spent outputs: {e}")))?;

            let (block_ref, txs) = check_full_block(
                block,
                prevouts,
                BlockExpectation {
                    network: self.chain.network(),
                    height,
                    // Both come from the header chain verified from genesis.
                    parent: Some(expected.prev_hash),
                    hash: Some(hash),
                },
                &mut seen_txids,
            )
            .map_err(at)?;
            blocks.push(BlockEvidence {
                block: block_ref,
                evidence: Evidence::FullBlock(txs),
            });
        }
        check_prevouts_within_range(&blocks).map_err(SourceError::InvalidData)?;
        Ok(ScanBatch { blocks })
    }

    fn stats(&self) -> SourceStats {
        self.http.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mainnet_and_unknown_chains_are_refused() {
        assert_eq!(
            network_from_core_chain("regtest").ok(),
            Some(LabNetwork::Regtest)
        );
        assert_eq!(
            network_from_core_chain("signet").ok(),
            Some(LabNetwork::Signet)
        );
        assert_eq!(
            network_from_core_chain("main"),
            Err(SourceError::Network(NetworkError::MainnetRefused))
        );
        for chain in ["test", "testnet4", "", "Main", "mainnet"] {
            assert!(
                matches!(
                    network_from_core_chain(chain),
                    Err(SourceError::Network(NetworkError::Unsupported(_)))
                ),
                "{chain}"
            );
        }
    }

    #[test]
    fn spent_outputs_decoding_is_strict() {
        // [[], [one P2TR output of 50 BTC]] as served by Bitcoin Core 32.1.
        let mut bytes = vec![0x02, 0x00, 0x01];
        bytes.extend_from_slice(&5_000_000_000u64.to_le_bytes());
        bytes.push(34);
        bytes.extend_from_slice(&[0x51, 0x20]);
        bytes.extend_from_slice(&[0xab; 32]);
        let decoded = decode_spent_outputs(&bytes).expect("valid");
        assert_eq!(decoded.len(), 2);
        assert!(decoded[0].is_empty());
        assert_eq!(decoded[1][0].value.to_sat(), 5_000_000_000);
        assert!(decoded[1][0].script_pubkey.is_p2tr());

        assert_eq!(decode_spent_outputs(&[0x00]).ok(), Some(vec![]));
        assert!(decode_spent_outputs(&[]).is_err());
        assert!(
            decode_spent_outputs(&bytes[..bytes.len() - 1]).is_err(),
            "truncated"
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_spent_outputs(&trailing).is_err(), "trailing byte");
    }

    /// The count limit itself (not end-of-input) rejects an absurd claim:
    /// enough bytes follow for every claimed transaction to decode.
    #[test]
    fn spent_outputs_transaction_count_is_bounded() {
        let encode = |count: u64| {
            let mut bytes = vec![0xfe];
            bytes.extend_from_slice(&(count as u32).to_le_bytes());
            bytes.resize(bytes.len() + count as usize, 0x00); // each tx: zero prevouts
            bytes
        };
        let at_limit = decode_spent_outputs(&encode(MAX_TXS_PER_BLOCK)).expect("at the limit");
        assert_eq!(at_limit.len() as u64, MAX_TXS_PER_BLOCK);
        let err = decode_spent_outputs(&encode(MAX_TXS_PER_BLOCK + 1)).expect_err("over the limit");
        assert!(err.contains("implausible transaction count"), "{err}");
    }
}
