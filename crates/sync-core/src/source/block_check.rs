//! Structural validation shared by every source that yields full blocks.
//!
//! These checks establish that a block is internally consistent and sits where
//! the caller expects it in the chain. They do **not** validate scripts,
//! signatures or amounts; whether the data is consensus-valid depends on where
//! it came from (see `SourceCapabilities::consensus_validated`).

use super::{coinbase_height, TxWithPrevouts};
use crate::{
    bitcoin::{hashes::Hash, Block, BlockHash, Target, TxOut, Txid},
    models::BlockRef,
    network::LabNetwork,
};
use std::collections::BTreeSet;

/// Where a block is expected to sit.
pub(crate) struct BlockExpectation {
    pub network: LabNetwork,
    pub height: u32,
    /// Hash the block's `prev_blockhash` must equal, when known.
    pub parent: Option<BlockHash>,
    /// Hash the block itself must have, when it was requested by hash.
    pub hash: Option<BlockHash>,
}

/// Validates `block` and pairs each transaction with its previous outputs.
/// `prevouts[t][i]` is spent by input `i` of transaction `t`; the coinbase
/// entry must be empty. `seen_txids` rejects a transaction that appears twice.
pub(crate) fn check_full_block(
    block: Block,
    prevouts: Vec<Vec<TxOut>>,
    expect: BlockExpectation,
    seen_txids: &mut BTreeSet<Txid>,
) -> Result<(BlockRef, Vec<TxWithPrevouts>), String> {
    let height = expect.height;
    if let Some(parent) = expect.parent {
        if block.header.prev_blockhash != parent {
            return Err(format!(
                "prev_blockhash {} does not link to parent {parent}",
                block.header.prev_blockhash
            ));
        }
    }
    if !block.check_merkle_root() {
        return Err("transactions do not match the header merkle root".to_string());
    }
    let hash = check_proof_of_work(&block, expect.network)
        .map_err(|e| format!("proof of work check failed: {e}"))?;
    if let Some(expected) = expect.hash {
        if hash != expected {
            return Err(format!("block hash is {hash} but {expected} was requested"));
        }
    }
    if !block.txdata.first().is_some_and(|tx| tx.is_coinbase()) {
        return Err("transaction 0 violates \"exactly one coinbase, first\"".to_string());
    }
    check_witness_commitment(&block).map_err(|e| format!("witness commitment: {e}"))?;
    match block.txdata.first().and_then(coinbase_height) {
        Some(h) if h == height => {}
        Some(h) => return Err(format!("coinbase commits to height {h}")),
        None => return Err("coinbase does not commit to a height".to_string()),
    }
    if prevouts.len() != block.txdata.len() {
        return Err(format!(
            "{} transactions but {} prevout lists",
            block.txdata.len(),
            prevouts.len()
        ));
    }

    let prev_hash = block.header.prev_blockhash;
    let mut txs = Vec::with_capacity(block.txdata.len());
    for (t, (tx, tx_prevouts)) in block.txdata.into_iter().zip(prevouts).enumerate() {
        let is_first = t == 0;
        if tx.is_coinbase() != is_first {
            return Err(format!(
                "transaction {t} violates \"exactly one coinbase, first\""
            ));
        }
        let txid = tx.compute_txid();
        if !seen_txids.insert(txid) {
            return Err(format!("duplicate transaction {txid}"));
        }
        let expected_prevouts = if is_first { 0 } else { tx.input.len() };
        if tx_prevouts.len() != expected_prevouts {
            return Err(format!(
                "transaction {txid} has {expected_prevouts} inputs needing prevouts but {} were supplied",
                tx_prevouts.len()
            ));
        }
        txs.push(TxWithPrevouts {
            tx,
            prevouts: tx_prevouts,
        });
    }

    Ok((
        BlockRef {
            height,
            hash,
            prev_hash,
        },
        txs,
    ))
}

/// Proof of work against the header's own `bits`, after checking that `bits`
/// is a canonical encoding and not easier than the network allows. (Without
/// that, a file could choose an arbitrarily easy or wrapped-around target.)
///
/// This still says nothing about the block being part of the most-work chain.
fn check_proof_of_work(block: &Block, network: LabNetwork) -> Result<BlockHash, String> {
    let bits = block.header.bits;
    let target = block.header.target();
    if target.to_compact_lossy() != bits {
        return Err(format!(
            "bits {:#010x} is not a canonical target encoding",
            bits.to_consensus()
        ));
    }
    let limit = match network {
        LabNetwork::Regtest => Target::MAX_ATTAINABLE_REGTEST,
        LabNetwork::Signet => Target::MAX_ATTAINABLE_SIGNET,
    };
    if target > limit {
        return Err(format!(
            "bits {:#010x} is easier than the {network} limit",
            bits.to_consensus()
        ));
    }
    block.header.validate_pow(target).map_err(|e| e.to_string())
}

/// BIP141 witness commitment, checked strictly.
///
/// `bitcoin::Block::check_witness_commitment` reports success for a block
/// whose inputs carry no witness at all, even when its coinbase commits to
/// witness data. Such a block is a *stripped* block: txids, merkle root and
/// proof of work are unchanged, but the data BIP352 needs is gone. Here a
/// commitment that is present must verify, and witness data without a
/// commitment is rejected.
fn check_witness_commitment(block: &Block) -> Result<(), String> {
    const MAGIC: [u8; 6] = [0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed];
    let coinbase = block.txdata.first().ok_or("block has no transactions")?;
    // BIP141: the commitment is the last matching output.
    let committed = coinbase
        .output
        .iter()
        .rev()
        .map(|o| o.script_pubkey.as_bytes())
        .find(|script| script.len() >= 38 && script[..6] == MAGIC)
        .map(|script| &script[6..38]);

    let Some(committed) = committed else {
        let has_witness = block
            .txdata
            .iter()
            .skip(1)
            .any(|tx| tx.input.iter().any(|i| !i.witness.is_empty()));
        return if has_witness {
            Err("transactions carry witness data but the coinbase has no commitment".to_string())
        } else {
            Ok(())
        };
    };

    let reserved = match coinbase.input.first().map(|i| i.witness.to_vec()) {
        Some(items) if items.len() == 1 && items[0].len() == 32 => items[0].clone(),
        _ => {
            return Err(
                "coinbase commits to witness data but lacks the 32-byte reserved value \
                 (block stripped of witnesses?)"
                    .to_string(),
            )
        }
    };
    let root = block
        .witness_root()
        .ok_or("cannot compute the witness root")?;
    let expected = Block::compute_witness_commitment(&root, &reserved);
    if expected.as_byte_array() != committed {
        return Err(
            "witness data does not match the coinbase commitment (modified or stripped witnesses)"
                .to_string(),
        );
    }
    Ok(())
}
