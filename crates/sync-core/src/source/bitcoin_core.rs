//! Bitcoin Core backend over the node's REST interface.
//!
//! Requires a node the user runs themselves, started with `-rest=1`, version
//! 30 or later (`/rest/spenttxouts`). Everything needed for BIP352 scanning is
//! read in binary form: the block with witnesses, and the outputs it spends.
//!
//! * No credentials: the REST interface is unauthenticated and read-only, so
//!   this adapter never sees an RPC cookie or password. The flip side is that
//!   anything on the same host can read the node's REST data too.
//! * Loopback only: the URL must be `http://` on `127.0.0.1`, `::1` or
//!   `localhost`. Nothing here talks to a third party.
//! * Mainnet is refused, both by chain name and by genesis hash.
//! * The node validated the data by consensus; this adapter additionally
//!   re-checks what it can check itself (hash, merkle root, witness
//!   commitment, proof of work, height, linkage, prevout counts), so that a
//!   block fetched without witnesses or a mismatched response is an error.
//!
//! Scan private keys never leave the process: requests contain heights and
//! block hashes only.

use super::{
    block_check::{check_full_block, BlockExpectation},
    BlockEvidence, Evidence, ScanBatch, ScanSource, SourceCapabilities, SourceError, SourceStats,
};
use crate::{
    bitcoin::{
        block::Header,
        consensus::{encode::VarInt, Decodable},
        Block, BlockHash, TxOut,
    },
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, LabNetwork, NetworkError},
};
use std::{
    collections::BTreeSet,
    net::IpAddr,
    sync::atomic::{AtomicU64, Ordering},
};

pub const CORE_REST_BACKEND_ID: &str = "bitcoin_core_rest";

/// Seconds to wait for one REST response.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Upper bound on transactions per block accepted from `spenttxouts`.
const MAX_TXS_PER_BLOCK: u64 = 1_000_000;

/// A Bitcoin Core node reached through its REST interface.
#[derive(Debug)]
pub struct CoreRestSource {
    /// `http://host:port`, no trailing slash.
    base: String,
    chain: ChainIdentity,
    timeout_secs: u64,
    requests: AtomicU64,
    bytes: AtomicU64,
}

#[derive(serde::Deserialize)]
struct ChainInfo {
    chain: String,
    blocks: u32,
    bestblockhash: BlockHash,
}

impl CoreRestSource {
    /// Connects to `rest_url` (e.g. `http://127.0.0.1:18443`), refusing
    /// non-loopback hosts, non-HTTP schemes and mainnet nodes.
    pub fn connect(rest_url: &str) -> Result<Self, SourceError> {
        let base = loopback_base_url(rest_url)?;
        let mut source = Self {
            base,
            // Placeholder until the node told us; replaced below before use.
            chain: ChainIdentity::for_network(LabNetwork::Regtest),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        };
        let info = source.chain_info()?;
        let network = network_from_core_chain(&info.chain)?;
        let genesis = source.block_hash_at(0)?;
        source.chain = ChainIdentity::verified(network, genesis)?;
        Ok(source)
    }

    fn get(&self, path: &str) -> Result<Vec<u8>, SourceError> {
        let url = format!("{}{path}", self.base);
        self.requests.fetch_add(1, Ordering::Relaxed);
        let response = minreq::get(&url)
            .with_timeout(self.timeout_secs)
            .with_max_redirects(0)
            .send()
            .map_err(|e| SourceError::Unavailable(format!("GET {path}: {e}")))?;
        let status = response.status_code;
        let body = response.into_bytes();
        self.bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
        if status != 200 {
            let text = String::from_utf8_lossy(&body);
            let text: String = text.trim().chars().take(200).collect();
            return Err(SourceError::Unavailable(format!(
                "GET {path}: HTTP {status}: {text}"
            )));
        }
        Ok(body)
    }

    fn chain_info(&self) -> Result<ChainInfo, SourceError> {
        let body = self.get("/rest/chaininfo.json")?;
        serde_json::from_slice(&body)
            .map_err(|e| SourceError::InvalidData(format!("chaininfo: {e}")))
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

    fn header(&self, hash: BlockHash) -> Result<Header, SourceError> {
        let body = self.get(&format!("/rest/headers/{hash}.bin?count=1"))?;
        let mut cursor: &[u8] = &body;
        let header = Header::consensus_decode(&mut cursor)
            .map_err(|e| SourceError::InvalidData(format!("header {hash}: {e}")))?;
        if !cursor.is_empty() || header.block_hash() != hash {
            return Err(SourceError::InvalidData(format!(
                "header {hash}: response is not exactly that header"
            )));
        }
        Ok(header)
    }
}

/// Maps Bitcoin Core's chain names. `main` is refused explicitly.
fn network_from_core_chain(chain: &str) -> Result<LabNetwork, SourceError> {
    match chain {
        "regtest" => Ok(LabNetwork::Regtest),
        "signet" => Ok(LabNetwork::Signet),
        "main" => Err(NetworkError::MainnetRefused.into()),
        other => Err(NetworkError::Unsupported(other.to_string()).into()),
    }
}

/// Validates `url` as `http://<loopback>[:port][/]` and returns it without a
/// trailing slash.
fn loopback_base_url(url: &str) -> Result<String, SourceError> {
    let refuse = |why: &str| {
        SourceError::InvalidData(format!(
            "refusing REST URL `{url}`: {why} (expected http://127.0.0.1:<port>)"
        ))
    };
    let rest = url
        .trim()
        .strip_prefix("http://")
        .ok_or_else(|| refuse("only plain http to the local node is supported"))?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    if authority.is_empty() || authority.contains(['/', '?', '#', '@', ' ']) {
        return Err(refuse("unexpected path, query or user info"));
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map(|(host, _)| host)
            .ok_or_else(|| refuse("malformed IPv6 host"))?
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    };
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
    if !is_loopback {
        return Err(refuse(
            "host is not loopback; remote nodes are out of scope",
        ));
    }
    Ok(format!("http://{authority}"))
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
            consensus_validated: true,
            remote: false,
        }
    }

    fn chain(&self) -> ChainIdentity {
        self.chain
    }

    fn tip(&self) -> Result<BlockRef, SourceError> {
        let info = self.chain_info()?;
        if network_from_core_chain(&info.chain)? != self.chain.network() {
            return Err(SourceError::InvalidData(format!(
                "node switched chain to `{}`",
                info.chain
            )));
        }
        let header = self.header(info.bestblockhash)?;
        Ok(BlockRef {
            height: info.blocks,
            hash: info.bestblockhash,
            prev_hash: header.prev_blockhash,
        })
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        let tip = self.chain_info()?.blocks;
        // The genesis block carries no BIP34 height and pays nobody.
        if from < 1 || to > tip {
            return Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: 1,
                available_to: tip,
            });
        }

        let mut blocks = Vec::new();
        let mut seen_txids = BTreeSet::new();
        let mut parent: Option<BlockHash> = None;
        for height in from..=to {
            let at = |e: String| SourceError::InvalidData(format!("block {height}: {e}"));
            let hash = self.block_hash_at(height)?;
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
                    // A reorg between two requests shows up here as a broken link.
                    parent,
                    hash: Some(hash),
                },
                &mut seen_txids,
            )
            .map_err(at)?;
            parent = Some(block_ref.hash);
            blocks.push(BlockEvidence {
                block: block_ref,
                evidence: Evidence::FullBlock(txs),
            });
        }
        Ok(ScanBatch { blocks })
    }

    fn stats(&self) -> SourceStats {
        SourceStats {
            requests: self.requests.load(Ordering::Relaxed),
            downloaded_bytes: self.bytes.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_to_loopback_is_accepted() {
        for (url, base) in [
            ("http://127.0.0.1:18443", "http://127.0.0.1:18443"),
            ("http://127.0.0.1:18443/", "http://127.0.0.1:18443"),
            ("http://localhost:38332", "http://localhost:38332"),
            ("http://[::1]:18443", "http://[::1]:18443"),
            (" http://127.0.0.9:1 ", "http://127.0.0.9:1"),
        ] {
            assert_eq!(loopback_base_url(url).ok().as_deref(), Some(base), "{url}");
        }
        for url in [
            "https://127.0.0.1:18443",
            "http://192.168.1.10:18443",
            "http://example.com:8332",
            "http://0.0.0.0:18443",
            "http://127.0.0.1.example.com:18443",
            "http://localhost.example.com",
            "http://user:pass@127.0.0.1:18443",
            "http://127.0.0.1:18443/rest",
            "http://127.0.0.1:18443/?x=1",
            "http://[2001:db8::1]:18443",
            "http://",
            "127.0.0.1:18443",
            "ftp://127.0.0.1",
        ] {
            assert!(loopback_base_url(url).is_err(), "{url} must be refused");
        }
    }

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
        // Claims far more transactions than a block can hold.
        assert!(decode_spent_outputs(&[0xfe, 0xff, 0xff, 0xff, 0x7f]).is_err());
    }
}
