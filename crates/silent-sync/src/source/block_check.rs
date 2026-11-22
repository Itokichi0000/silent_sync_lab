//! Structural validation shared by every source that yields full blocks.
//!
//! These checks establish that a block is internally consistent and sits where
//! the caller expects it in the chain. They do **not** validate scripts,
//! signatures or amounts; whether the data is consensus-valid depends on where
//! it came from (see `SourceCapabilities::consensus_validated`).

use super::{coinbase_height, TxWithPrevouts};
use crate::{
    bitcoin::{block::Header, hashes::Hash, Block, BlockHash, Target, TxOut, Txid},
    models::BlockRef,
    network::Network,
};
use std::collections::BTreeSet;

/// Where a block is expected to sit.
pub(crate) struct BlockExpectation {
    pub network: Network,
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
    let hash = check_proof_of_work(&block.header, expect.network)
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
pub(crate) fn check_proof_of_work(header: &Header, network: Network) -> Result<BlockHash, String> {
    let bits = header.bits;
    let target = header.target();
    if target.to_compact_lossy() != bits {
        return Err(format!(
            "bits {:#010x} is not a canonical target encoding",
            bits.to_consensus()
        ));
    }
    let limit = match network {
        Network::Regtest => Target::MAX_ATTAINABLE_REGTEST,
        Network::Signet => Target::MAX_ATTAINABLE_SIGNET,
        #[cfg(feature = "mainnet")]
        Network::Mainnet => Target::MAX_ATTAINABLE_MAINNET,
    };
    if target > limit {
        return Err(format!(
            "bits {:#010x} is easier than the {network} limit",
            bits.to_consensus()
        ));
    }
    header.validate_pow(target).map_err(|e| e.to_string())
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

/// Cross-checks supplied previous outputs against the outputs that were
/// created inside the same batch.
///
/// Nothing in a block commits to the outputs it spends, so in general a
/// source's prevouts cannot be verified. The exception is an input spending an
/// output of an earlier transaction in the fetched range: there the real
/// output is known, and a prevout that differs is proof the source lied (or
/// mixed data up). A wrong prevout silently changes — usually destroys — the
/// BIP352 shared secret, i.e. it hides payments.
pub(crate) fn check_prevouts_within_range(blocks: &[super::BlockEvidence]) -> Result<(), String> {
    use crate::bitcoin::OutPoint;
    use std::collections::BTreeMap;

    let mut created: BTreeMap<OutPoint, &TxOut> = BTreeMap::new();
    for block in blocks {
        let super::Evidence::FullBlock(txs) = &block.evidence else {
            continue;
        };
        for entry in txs {
            for (txin, supplied) in entry.tx.input.iter().zip(&entry.prevouts) {
                if let Some(real) = created.get(&txin.previous_output) {
                    if *real != supplied {
                        return Err(format!(
                            "block {}: transaction {} spends {} but the supplied previous output \
                             differs from the output created in this range",
                            block.block.height,
                            entry.tx.compute_txid(),
                            txin.previous_output
                        ));
                    }
                }
            }
            let txid = entry.tx.compute_txid();
            for (vout, output) in entry.tx.output.iter().enumerate() {
                if let Ok(vout) = u32::try_from(vout) {
                    created.insert(OutPoint::new(txid, vout), output);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bitcoin::{
            absolute::LockTime, transaction::Version, Amount, OutPoint, ScriptBuf, Sequence,
            Transaction, TxIn, Witness,
        },
        source::{BlockEvidence, Evidence},
    };

    fn tx(inputs: &[OutPoint], outputs: &[(u64, u8)]) -> Transaction {
        Transaction {
            version: Version::TWO,
            lock_time: LockTime::ZERO,
            input: inputs
                .iter()
                .map(|&previous_output| TxIn {
                    previous_output,
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::MAX,
                    witness: Witness::new(),
                })
                .collect(),
            output: outputs
                .iter()
                .map(|&(sats, tag)| TxOut {
                    value: Amount::from_sat(sats),
                    script_pubkey: ScriptBuf::from_bytes(vec![0x51, tag]),
                })
                .collect(),
        }
    }

    fn block(height: u32, txs: Vec<TxWithPrevouts>) -> BlockEvidence {
        let hash = BlockHash::from_byte_array([height as u8; 32]);
        BlockEvidence {
            block: BlockRef {
                height,
                hash,
                prev_hash: hash,
            },
            evidence: Evidence::FullBlock(txs),
        }
    }

    /// An input that spends an output created earlier in the same batch must
    /// be given exactly that output as its prevout.
    #[test]
    fn prevouts_are_cross_checked_against_outputs_created_in_the_range() {
        let outside = OutPoint::new(crate::bitcoin::Txid::from_byte_array([9; 32]), 0);
        let funding = tx(&[outside], &[(1_000, 1), (2_000, 2)]);
        let spent = OutPoint::new(funding.compute_txid(), 1);
        let spender = tx(&[outside, spent], &[(2_500, 3)]);
        let unknown = TxOut {
            value: Amount::from_sat(777),
            script_pubkey: ScriptBuf::new(),
        };
        let build = |supplied: TxOut, same_block: bool| {
            let first = TxWithPrevouts {
                tx: funding.clone(),
                prevouts: vec![unknown.clone()],
            };
            let second = TxWithPrevouts {
                tx: spender.clone(),
                // The first prevout is from outside the range: unverifiable.
                prevouts: vec![unknown.clone(), supplied],
            };
            if same_block {
                vec![block(5, vec![first, second])]
            } else {
                vec![block(5, vec![first]), block(6, vec![second])]
            }
        };
        let real = funding.output[1].clone();
        for same_block in [true, false] {
            assert_eq!(
                check_prevouts_within_range(&build(real.clone(), same_block)),
                Ok(())
            );
            for lie in [
                funding.output[0].clone(), // the wrong output of the right transaction
                TxOut {
                    value: Amount::from_sat(2_001),
                    ..real.clone()
                },
                TxOut {
                    script_pubkey: ScriptBuf::new(),
                    ..real.clone()
                },
            ] {
                let err = check_prevouts_within_range(&build(lie, same_block)).expect_err("lie");
                assert!(
                    err.contains("differs from the output created in this range"),
                    "{err}"
                );
                assert!(err.contains(&spent.to_string()), "{err}");
            }
        }
    }

    /// Mainnet's block 1 (public data). It meets mainnet's limit; a header
    /// mined at regtest's difficulty does not, even though its own proof of
    /// work is valid. And mainnet's limit is not regtest's: the same header
    /// passes there.
    #[cfg(feature = "mainnet")]
    #[test]
    fn mainnet_has_its_own_proof_of_work_limit() {
        use crate::bitcoin::{
            blockdata::constants::genesis_block, consensus::deserialize, hex::FromHex,
        };

        let bytes = Vec::<u8>::from_hex(
            "010000006fe28c0ab6f1b372c1a6a246ae63f74f931e8365e15a089c68d6190000000000\
             982051fd1e4ba744bbbe680e1fee14677ba1a3c3540bf7b1cdb606e857233e0e\
             61bc6649ffff001d01e36299",
        )
        .expect("hex");
        let block_1: Header = deserialize(&bytes).expect("header");
        assert_eq!(block_1.prev_blockhash, Network::Mainnet.genesis_hash());
        assert_eq!(
            check_proof_of_work(&block_1, Network::Mainnet).map(|h| h.to_string()),
            Ok("00000000839a8e6886ab5951d76f411475428afc90947ee320161bbf18eb6048".to_string())
        );

        let easy = genesis_block(Network::Regtest.to_bitcoin()).header;
        assert!(check_proof_of_work(&easy, Network::Regtest).is_ok());
        let refused = check_proof_of_work(&easy, Network::Mainnet).expect_err("too easy");
        assert!(
            refused.contains("easier than the mainnet limit"),
            "{refused}"
        );
    }
}
