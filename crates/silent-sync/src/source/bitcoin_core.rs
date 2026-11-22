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
    network::{ChainIdentity, Network, NetworkError},
};
use std::{collections::BTreeSet, sync::Mutex};

const CORE_REST_BACKEND_ID: &str = "bitcoin_core_rest";

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
    /// non-loopback hosts and non-HTTP schemes, and — unless the crate is
    /// built with the `mainnet` feature — mainnet nodes.
    ///
    /// The network is the one the node is on. Compare
    /// [`ScanSource::chain`] with the network you expect before using the
    /// source; `sync` does so against the store's binding.
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
        let genesis = source.block_hash_at(0)?.ok_or_else(|| {
            SourceError::InvalidData("the node reports no block at height 0".to_string())
        })?;
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

    /// The hash the node has at `height` on its active chain, or `None` if
    /// the node *answers* that its chain does not reach that height (HTTP
    /// 404). A node that does not answer is an error, never "no such block".
    fn block_hash_at(&self, height: u32) -> Result<Option<BlockHash>, SourceError> {
        let Some(body) = self
            .http
            .get_if_found(&format!("/rest/blockhashbyheight/{height}.hex"))?
        else {
            return Ok(None);
        };
        std::str::from_utf8(&body)
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .map(Some)
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

    /// Makes sure the verified header chain reaches `to` on the node's
    /// *current* chain and returns the verified hashes of heights
    /// `from..=to` (`from` may be 0, the genesis block).
    ///
    /// Stale entries (after a reorg on the node) are dropped first; new ones
    /// are added only by following `prev_blockhash` links from an already
    /// verified hash, with proof of work checked for every header.
    fn anchor(&self, from: u32, to: u32) -> Result<Vec<BlockHash>, SourceError> {
        let height = to;
        let mut anchored = self
            .anchored
            .lock()
            .map_err(|_| SourceError::Unavailable("internal lock poisoned".to_string()))?;
        let network = self.chain.network();

        // Drop whatever is no longer on the node's active chain. Hashes are
        // linked, so "the node has our hash at height h" holds for a prefix
        // of the cache: the fork point is found by bisection. Only a definite
        // answer from the node (another hash, or "no such height") removes
        // entries; a node that cannot be reached fails the call and leaves
        // the verified cache as it is.
        let on_node = |h: usize, anchored: &[BlockHash]| -> Result<bool, SourceError> {
            Ok(self.block_hash_at(h as u32)? == Some(anchored[h]))
        };
        let top = anchored.len() - 1;
        if top > 0 && !on_node(top, &anchored)? {
            // Invariant: `low` is on the node's chain (genesis by construction
            // of the chain identity), `high` is not.
            let (mut low, mut high) = (0usize, top);
            while high - low > 1 {
                let mid = low + (high - low) / 2;
                if on_node(mid, &anchored)? {
                    low = mid;
                } else {
                    high = mid;
                }
            }
            anchored.truncate(low + 1);
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
                    from: from.max(1),
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
        Ok(anchored[from as usize..=height as usize].to_vec())
    }
}

fn chain_info(http: &LoopbackHttp) -> Result<ChainInfo, SourceError> {
    let body = http.get("/rest/chaininfo.json")?;
    serde_json::from_slice(&body).map_err(|e| {
        SourceError::InvalidData(format!(
            "chaininfo: {}",
            super::http::printable(&e.to_string())
        ))
    })
}

/// Maps Bitcoin Core's chain names. `main` is refused explicitly unless the
/// crate is built with the `mainnet` feature.
fn network_from_core_chain(chain: &str) -> Result<Network, SourceError> {
    match chain {
        "regtest" => Ok(Network::Regtest),
        "signet" => Ok(Network::Signet),
        "main" => Ok(crate::network::mainnet()?),
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

impl CoreRestSource {
    /// Verified block references for `from..=to`, from one validation of the
    /// header anchor.
    fn verified_refs(&self, from: u32, to: u32) -> Result<Vec<BlockRef>, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        // One more below `from`, for the first block's parent.
        let first = from.saturating_sub(1);
        let hashes = self.anchor(first, to)?;
        Ok((from..=to)
            .map(|height| {
                let index = (height - first) as usize;
                BlockRef {
                    height,
                    hash: hashes[index],
                    // The genesis block's parent is the all-zero hash.
                    prev_hash: if height == 0 {
                        BlockHash::all_zeros()
                    } else {
                        hashes[index - 1]
                    },
                }
            })
            .collect())
    }

    fn verified_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        self.verified_refs(height, height)?
            .pop()
            .ok_or_else(|| SourceError::InvalidData("empty header anchor".to_string()))
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
        let tip = self.verified_ref(info.blocks)?;
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
        self.verified_ref(height)
    }

    fn block_refs(&self, from: u32, to: u32) -> Result<Vec<BlockRef>, SourceError> {
        self.verified_refs(from, to)
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
        let verified = self.verified_refs(from, to)?;

        let mut blocks = Vec::new();
        let mut seen_txids = BTreeSet::new();
        for expected in verified {
            let height = expected.height;
            let at = |e: String| SourceError::InvalidData(format!("block {height}: {e}"));
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
            Some(Network::Regtest)
        );
        assert_eq!(
            network_from_core_chain("signet").ok(),
            Some(Network::Signet)
        );
        #[cfg(not(feature = "mainnet"))]
        assert_eq!(
            network_from_core_chain("main"),
            Err(SourceError::Network(NetworkError::MainnetRefused))
        );
        #[cfg(feature = "mainnet")]
        assert_eq!(network_from_core_chain("main").ok(), Some(Network::Mainnet));
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
