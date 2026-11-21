//! BlindBit Oracle v2 backend: a tweak index over HTTP, paired with an
//! independent header source.
//!
//! This is the light-client arrangement the lab compares against scanning
//! full blocks: block *identity* comes from a source the user controls
//! (headers of their own node), and the *scan data* — which transactions are
//! scannable, their tweak data, their taproot outputs — comes from the index.
//!
//! What the pairing gives:
//! * the oracle's network name must match (`main` is refused), and for every
//!   block its reported hash must equal the header source's hash at that
//!   height, so an index for another chain, a stale index or an index that is
//!   behind is an error rather than "no payments";
//! * in particular the oracle's habit of answering heights it does not have
//!   with HTTP 200 and an empty hash (observed, see `docs/RESEARCH.md` §4.1)
//!   cannot be mistaken for an empty block.
//!
//! What it cannot give: any assurance about the index *content*. A missing
//! transaction, a wrong tweak, a fabricated output — none of that is
//! detectable from here. See `verify` for comparing against a full-block
//! source.
//!
//! Only the oracle's HTTP JSON interface is used (`/info`, `/full-block/<h>`).
//! Upstream marks the JSON block endpoints as deprecated in favour of gRPC;
//! they were observed working at commit `f9ab7fd`. Requests carry a block
//! height and nothing else. Loopback only, like every network backend here.

use super::{
    http::{printable, LoopbackHttp},
    BlockEvidence, Evidence, IndexedOutput, IndexedTx, ScanBatch, ScanSource, SourceCapabilities,
    SourceError, SourceStats,
};
use crate::{
    bitcoin::{hex::FromHex, secp256k1::PublicKey, BlockHash, Txid},
    models::{BlockRef, EvidenceKind},
    network::{ChainIdentity, LabNetwork, NetworkError},
};
use serde::Deserialize;

pub const BLINDBIT_BACKEND_ID: &str = "blindbit_oracle_v2_http";

/// A BlindBit Oracle v2 instance plus the header source that anchors it.
pub struct BlindbitHttpSource<H> {
    http: LoopbackHttp,
    headers: H,
}

#[derive(Deserialize)]
struct Info {
    network: String,
    height: u32,
    #[serde(default)]
    tweaks_only: bool,
}

#[derive(Deserialize)]
struct BlockIdentifier {
    block_hash: String,
    block_height: u32,
}

#[derive(Deserialize)]
struct FullBlock {
    block_identifier: BlockIdentifier,
    /// `null` for heights the oracle has no data for.
    index: Option<Vec<FullBlockTx>>,
}

#[derive(Deserialize)]
struct FullBlockTx {
    txid: Txid,
    tweak: PublicKey,
    #[serde(default)]
    utxos: Vec<FullBlockUtxo>,
}

#[derive(Deserialize)]
struct FullBlockUtxo {
    vout: u32,
    amount: u64,
    pubkey: String,
}

fn network_from_oracle(name: &str) -> Result<LabNetwork, SourceError> {
    match name {
        "regtest" => Ok(LabNetwork::Regtest),
        "signet" => Ok(LabNetwork::Signet),
        "main" | "mainnet" => Err(NetworkError::MainnetRefused.into()),
        other => Err(NetworkError::Unsupported(printable(other)).into()),
    }
}

impl<H: ScanSource> BlindbitHttpSource<H> {
    /// Connects to the oracle at `oracle_url` (loopback only). `headers`
    /// supplies block identity; it must be on the same network.
    pub fn connect(oracle_url: &str, headers: H) -> Result<Self, SourceError> {
        let source = Self {
            http: LoopbackHttp::new(oracle_url, "oracle")?,
            headers,
        };
        let info = source.info()?;
        let network = network_from_oracle(&info.network)?;
        if network != source.headers.chain().network() {
            return Err(SourceError::InvalidData(format!(
                "the oracle indexes {network} but the header source is on {}",
                source.headers.chain().network()
            )));
        }
        if info.tweaks_only {
            return Err(SourceError::InvalidData(
                "the oracle runs with tweaks_only: it serves no outputs, so detections cannot be \
                 located"
                    .to_string(),
            ));
        }
        Ok(source)
    }

    pub fn header_source(&self) -> &H {
        &self.headers
    }

    fn info(&self) -> Result<Info, SourceError> {
        let body = self.http.get("/info")?;
        serde_json::from_slice(&body)
            .map_err(|e| SourceError::InvalidData(format!("oracle /info: {e}")))
    }

    /// The oracle's index for `expected`, after checking it is for that block.
    fn indexed_block(&self, expected: BlockRef) -> Result<Vec<IndexedTx>, SourceError> {
        let height = expected.height;
        let at = |e: String| SourceError::InvalidData(format!("oracle block {height}: {e}"));
        let body = self.http.get(&format!("/full-block/{height}"))?;
        let parsed: FullBlock = serde_json::from_slice(&body).map_err(|e| at(e.to_string()))?;

        if parsed.block_identifier.block_height != height {
            return Err(at(format!(
                "answer is for height {}",
                parsed.block_identifier.block_height
            )));
        }
        if parsed.block_identifier.block_hash.is_empty() {
            return Err(at(
                "the oracle has no data for this height (not synced that far, or below its start height)"
                    .to_string(),
            ));
        }
        let hash: BlockHash = parsed
            .block_identifier
            .block_hash
            .parse()
            .map_err(|_| at("block hash is not a hash".to_string()))?;
        if hash != expected.hash {
            return Err(at(format!(
                "the oracle indexed block {hash}, but the header source has {} at this height",
                expected.hash
            )));
        }

        parsed
            .index
            .unwrap_or_default()
            .into_iter()
            .map(|tx| {
                let outputs = tx
                    .utxos
                    .into_iter()
                    .map(|o| {
                        <[u8; 32]>::from_hex(&o.pubkey)
                            .map(|output_key| IndexedOutput {
                                vout: o.vout,
                                value_sats: o.amount,
                                output_key,
                            })
                            .map_err(|_| at(format!("transaction {}: bad output key", tx.txid)))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(IndexedTx {
                    txid: tx.txid,
                    tweak: tx.tweak,
                    outputs,
                })
            })
            .collect()
    }
}

impl<H: ScanSource> ScanSource for BlindbitHttpSource<H> {
    fn backend_id(&self) -> &str {
        BLINDBIT_BACKEND_ID
    }

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            evidence_kind: EvidenceKind::TweakIndex,
            // Block identity is anchored by the header source; the index
            // content is not verified against the block in any way.
            verifies_merkle_root: false,
            verifies_witness_commitment: false,
            verifies_proof_of_work: false,
            consensus_validated: false,
            // Loopback only for now; a hosted oracle would additionally see
            // the client's address, timing and the ranges it asks for.
            remote: false,
        }
    }

    fn chain(&self) -> ChainIdentity {
        self.headers.chain()
    }

    /// The header source's tip. Syncing to it fails (rather than stopping
    /// early) if the oracle has not indexed that far.
    fn tip(&self) -> Result<BlockRef, SourceError> {
        self.headers.tip()
    }

    fn block_ref(&self, height: u32) -> Result<BlockRef, SourceError> {
        self.headers.block_ref(height)
    }

    fn fetch_range(&self, from: u32, to: u32) -> Result<ScanBatch, SourceError> {
        if from > to {
            return Err(SourceError::InvalidRange { from, to });
        }
        let indexed_to = self.info()?.height;
        if from < 1 || to > indexed_to {
            return Err(SourceError::RangeUnavailable {
                from,
                to,
                available_from: 1,
                available_to: indexed_to,
            });
        }
        let mut blocks = Vec::new();
        for height in from..=to {
            let block = self.headers.block_ref(height)?;
            let indexed = self.indexed_block(block)?;
            blocks.push(BlockEvidence {
                block,
                evidence: Evidence::TweakIndex(indexed),
            });
        }
        Ok(ScanBatch { blocks })
    }

    /// Transfer counters of the oracle connection only (the header source
    /// keeps its own).
    fn stats(&self) -> SourceStats {
        self.http.stats()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_network_names() {
        assert_eq!(
            network_from_oracle("regtest").ok(),
            Some(LabNetwork::Regtest)
        );
        assert_eq!(network_from_oracle("signet").ok(), Some(LabNetwork::Signet));
        for name in ["main", "mainnet"] {
            assert_eq!(
                network_from_oracle(name),
                Err(SourceError::Network(NetworkError::MainnetRefused))
            );
        }
        for name in ["testnet", "", "Main", "regtest "] {
            assert!(matches!(
                network_from_oracle(name),
                Err(SourceError::Network(NetworkError::Unsupported(_)))
            ));
        }
    }
}
