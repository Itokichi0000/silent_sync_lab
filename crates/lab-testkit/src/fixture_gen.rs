//! Deterministic generator for `fixtures/local/regtest_basic_v1.json`.
//!
//! TEST-DATA GENERATION ONLY. This is the one place in the repository that
//! uses upstream's *sending* functions; it lives in the test-only crate
//! `lab-testkit`, never in `sync-core` or the CLI.
//!
//! Key material:
//! * receiver keys are the public BIP352 test-vector keys, read from the
//!   pinned vector file (no new secret is introduced);
//! * sender keys are derived from fixed public strings and control nothing —
//!   the outputs they "spend" exist on no chain.
//!
//! The generated transactions carry placeholder signatures and are **not**
//! consensus-valid. Blocks have valid regtest proof of work, merkle roots and
//! witness commitments so that the fixture adapter's structural checks are
//! exercised for real.

use bdk_sp::{
    encoding::SilentPaymentCode,
    send::{create_silentpayment_partial_secret, create_silentpayment_scriptpubkeys},
    LexMin,
};
use std::collections::HashMap;
use sync_core::{
    bitcoin::{
        absolute::LockTime,
        block::{Header, Version as BlockVersion},
        consensus::encode::serialize_hex,
        hashes::{sha256, Hash},
        key::{CompressedPublicKey, Secp256k1, TweakedPublicKey},
        opcodes::all::{OP_PUSHNUM_1, OP_RETURN},
        script::Builder,
        secp256k1::{PublicKey, SecretKey},
        transaction::Version,
        Amount, Block, BlockHash, CompactTarget, Network, OutPoint, ScriptBuf, Sequence,
        Transaction, TxIn, TxMerkleNode, TxOut, Txid, Witness, WitnessProgram, WitnessVersion,
        XOnlyPublicKey,
    },
    network::LabNetwork,
    protocol::{LabAddress, ReceiverKeys},
    source::fixture::{
        FixtureBlock, FixtureExpectedDetection, FixtureFile, FixturePrevout, FIXTURE_FORMAT,
    },
};

pub const VECTORS_JSON: &str =
    include_str!("../../../fixtures/bip352/send_and_receive_test_vectors.json");

/// Label integers the lab receiver hands out in this fixture.
pub const LABEL_USED: u32 = 1;
/// A label that was used by a sender but that a restoring wallet does not know
/// about unless its labels were backed up.
pub const LABEL_NOT_BACKED_UP: u32 = 5;

const TAG: &str = "silent-sync-lab/fixture-v1";
const PREVOUT_VALUE: u64 = 100_000;
const REGTEST_BITS: u32 = 0x207f_ffff;
const BASE_TIME: u32 = 1_700_000_000;

/// Raw receiver key material from the BIP352 vector file (scan private key,
/// spend public key) of the vector at `index`, receiver `receiver`.
/// Tests use this to drive the upstream scanner directly for comparison.
pub fn vector_raw_keys(index: usize, receiver: usize) -> (SecretKey, PublicKey) {
    let all: serde_json::Value = serde_json::from_str(VECTORS_JSON).unwrap();
    let material = &all[index]["receiving"][receiver]["given"]["key_material"];
    let scan: SecretKey = material["scan_priv_key"].as_str().unwrap().parse().unwrap();
    let spend: SecretKey = material["spend_priv_key"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (scan, spend.public_key(&Secp256k1::new()))
}

/// Receiver keys of the vector at `index`, receiver `receiver`.
pub fn vector_receiver_keys(index: usize, receiver: usize) -> ReceiverKeys {
    let (scan, spend) = vector_raw_keys(index, receiver);
    ReceiverKeys::new(scan, spend)
}

/// (vector index, receiver index) of the lab's fixed test receiver.
pub const LAB_RECEIVER: (usize, usize) = (0, 0);
/// (vector index, receiver index) of an unrelated receiver.
pub const OTHER_RECEIVER: (usize, usize) = (18, 0);

/// The lab's fixed test receiver.
pub fn lab_receiver_keys() -> ReceiverKeys {
    vector_receiver_keys(LAB_RECEIVER.0, LAB_RECEIVER.1)
}

/// A different receiver, used to check that nothing is detected with other keys.
pub fn other_receiver_keys() -> ReceiverKeys {
    vector_receiver_keys(OTHER_RECEIVER.0, OTHER_RECEIVER.1)
}

fn det_bytes(what: &str) -> [u8; 32] {
    sha256::Hash::hash(format!("{TAG}/{what}").as_bytes()).to_byte_array()
}

fn sender_key(i: u32) -> SecretKey {
    SecretKey::from_slice(&det_bytes(&format!("sender-key/{i}"))).unwrap()
}

fn funding_outpoint(i: u32) -> OutPoint {
    OutPoint::new(
        Txid::from_byte_array(det_bytes(&format!("funding/{i}"))),
        i % 3,
    )
}

fn unrelated_p2tr(i: u32) -> ScriptBuf {
    let (key, _) = sender_key(1_000 + i).x_only_public_key(&Secp256k1::new());
    ScriptBuf::new_p2tr_tweaked(TweakedPublicKey::dangerous_assume_tweaked(key))
}

fn p2wpkh_script(sk: &SecretKey) -> ScriptBuf {
    let pk = CompressedPublicKey(sk.public_key(&Secp256k1::new()));
    ScriptBuf::new_p2wpkh(&pk.wpubkey_hash())
}

fn p2tr_from_x_only(key: XOnlyPublicKey) -> ScriptBuf {
    ScriptBuf::new_p2tr_tweaked(TweakedPublicKey::dangerous_assume_tweaked(key))
}

#[derive(Clone, Copy)]
enum InputKind {
    /// Eligible: P2WPKH.
    P2wpkh,
    /// Eligible: P2TR key-path spend.
    P2trKeyPath,
    /// Not eligible: P2WSH.
    P2wsh,
    /// SegWit v2 prevout: makes the whole transaction non-scannable.
    SegwitV2,
}

struct Input {
    txin: TxIn,
    prevout: TxOut,
    /// Present for inputs that contribute to the shared secret.
    key: Option<SecretKey>,
}

fn input(i: u32, kind: InputKind) -> Input {
    let secp = Secp256k1::new();
    let sk = sender_key(i);
    let (script_pubkey, witness, key) = match kind {
        InputKind::P2wpkh => {
            let pk: PublicKey = sk.public_key(&secp);
            (
                p2wpkh_script(&sk),
                Witness::from_slice(&[vec![0x30; 71], pk.serialize().to_vec()]),
                Some(sk),
            )
        }
        InputKind::P2trKeyPath => {
            let (x_only, _) = sk.x_only_public_key(&secp);
            (
                p2tr_from_x_only(x_only),
                Witness::from_slice(&[vec![0u8; 64]]),
                Some(sk),
            )
        }
        InputKind::P2wsh => {
            let witness_script = Builder::new().push_opcode(OP_PUSHNUM_1).into_script();
            (
                ScriptBuf::new_p2wsh(&witness_script.wscript_hash()),
                Witness::from_slice(&[witness_script.to_bytes()]),
                None,
            )
        }
        InputKind::SegwitV2 => (
            ScriptBuf::new_witness_program(
                &WitnessProgram::new(WitnessVersion::V2, &det_bytes(&format!("v2/{i}"))).unwrap(),
            ),
            Witness::from_slice(&[vec![0x01]]),
            None,
        ),
    };
    Input {
        txin: TxIn {
            previous_output: funding_outpoint(i),
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness,
        },
        prevout: TxOut {
            value: Amount::from_sat(PREVOUT_VALUE),
            script_pubkey,
        },
        key,
    }
}

/// Silent payment output scripts for `codes`, in the order given, derived by
/// upstream's sending functions from the eligible `inputs`.
fn silent_payment_scripts(inputs: &[Input], codes: &[SilentPaymentCode]) -> Vec<ScriptBuf> {
    let mut lex_min = LexMin::default();
    let mut spks_with_keys = Vec::new();
    for i in inputs {
        lex_min.update(&i.txin.previous_output);
        if let Some(key) = i.key {
            spks_with_keys.push((i.prevout.script_pubkey.clone(), key));
        }
    }
    let partial_secret =
        create_silentpayment_partial_secret(&lex_min.bytes().unwrap(), &spks_with_keys).unwrap();
    let mut by_code: HashMap<SilentPaymentCode, Vec<XOnlyPublicKey>> =
        create_silentpayment_scriptpubkeys(partial_secret, codes);
    codes
        .iter()
        .map(|code| p2tr_from_x_only(by_code.get_mut(code).unwrap().remove(0)))
        .collect()
}

fn tx(inputs: &[Input], outputs: Vec<(u64, ScriptBuf)>) -> (Transaction, Vec<FixturePrevout>) {
    let tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs.iter().map(|i| i.txin.clone()).collect(),
        output: outputs
            .into_iter()
            .map(|(sats, script_pubkey)| TxOut {
                value: Amount::from_sat(sats),
                script_pubkey,
            })
            .collect(),
    };
    let prevouts = inputs
        .iter()
        .map(|i| FixturePrevout {
            value_sats: i.prevout.value.to_sat(),
            script_pubkey_hex: i.prevout.script_pubkey.to_hex_string(),
        })
        .collect();
    (tx, prevouts)
}

fn coinbase(height: u32) -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: Builder::new()
                .push_int(i64::from(height))
                .push_slice(b"silent-sync-lab")
                .into_script(),
            sequence: Sequence::MAX,
            // BIP141 witness reserved value.
            witness: Witness::from_slice(&[[0u8; 32]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(50 * 100_000_000),
            script_pubkey: Builder::new().push_opcode(OP_PUSHNUM_1).into_script(),
        }],
    }
}

fn is_witness_commitment(output: &TxOut) -> bool {
    output
        .script_pubkey
        .as_bytes()
        .starts_with(&[0x6a, 0x24, 0xaa, 0x21, 0xa9, 0xed])
}

/// Makes `block` internally consistent again after its transactions or header
/// changed: BIP141 witness commitment, merkle root and regtest proof of work.
/// Tests use this to build blocks that are wrong in exactly one respect.
pub fn seal(block: &mut Block) {
    // BIP141 commitment output: OP_RETURN 0xaa21a9ed || commitment.
    let witness_root = block.witness_root().unwrap();
    let commitment = Block::compute_witness_commitment(&witness_root, &[0u8; 32]);
    let mut payload = [0u8; 36];
    payload[..4].copy_from_slice(&[0xaa, 0x21, 0xa9, 0xed]);
    payload[4..].copy_from_slice(commitment.as_byte_array());
    let coinbase = &mut block.txdata[0];
    coinbase.output.retain(|o| !is_witness_commitment(o));
    coinbase.output.push(TxOut {
        value: Amount::ZERO,
        script_pubkey: Builder::new()
            .push_opcode(OP_RETURN)
            .push_slice(payload)
            .into_script(),
    });
    block.header.merkle_root = block.compute_merkle_root().unwrap();
    block.header.nonce = 0;
    while block.header.validate_pow(block.header.target()).is_err() {
        block.header.nonce += 1;
    }
}

fn block(height: u32, prev: BlockHash, txs: Vec<Transaction>) -> Block {
    let mut txdata = vec![coinbase(height)];
    txdata.extend(txs);
    let mut block = Block {
        header: Header {
            version: BlockVersion::from_consensus(0x2000_0000),
            prev_blockhash: prev,
            merkle_root: TxMerkleNode::all_zeros(),
            time: BASE_TIME + height * 600,
            bits: CompactTarget::from_consensus(REGTEST_BITS),
            nonce: 0,
        },
        txdata,
    };
    seal(&mut block);
    block
}

/// Builds the fixture. Pure and deterministic: the same output on every call.
pub fn generate() -> FixtureFile {
    build(false)
}

/// A competing chain for reorg tests. Heights 1–5 are byte-identical to
/// [`generate`]; from height 6 on it differs and it is one block longer:
///
/// * 6': a payment to the receiver that exists only on this branch;
/// * 7': coinbase only (the main chain's change-only payment is gone);
/// * 8': the *same* 1 sat transaction as the main chain's height 8, now in a
///   different block;
/// * 9': coinbase only.
pub fn generate_fork() -> FixtureFile {
    build(true)
}

/// Height up to which [`generate`] and [`generate_fork`] are identical.
pub const FORK_HEIGHT: u32 = 5;

fn build(fork: bool) -> FixtureFile {
    let network = LabNetwork::Regtest;
    let receiver = lab_receiver_keys();
    let other = other_receiver_keys();
    // Upstream's address type, needed by upstream's sending functions.
    let code = |a: LabAddress| {
        SilentPaymentCode::new_v0(a.scan_public_key(), a.spend_public_key(), Network::Regtest)
    };
    let plain = code(receiver.address(network));
    let label_used = code(receiver.labelled_address(network, LABEL_USED).unwrap());
    let label_change = code(receiver.labelled_address(network, 0).unwrap());
    let label_lost = code(
        receiver
            .labelled_address(network, LABEL_NOT_BACKED_UP)
            .unwrap(),
    );

    let mut blocks: Vec<FixtureBlock> = Vec::new();
    let mut expected: Vec<FixtureExpectedDetection> = Vec::new();
    let mut prev = network.genesis_hash();

    // (transaction, prevouts, [(vout, k, label, note)] created for the receiver)
    type Planned = (
        Transaction,
        Vec<FixturePrevout>,
        Vec<(u32, u32, Option<u32>, &'static str)>,
    );
    let mut plan: Vec<Vec<Planned>> = Vec::new();

    // Height 1: coinbase only.
    plan.push(vec![]);

    // Height 2: one payment, one input (T02), next to an unrelated P2TR output.
    {
        let ins = [input(0, InputKind::P2wpkh)];
        let sp = silent_payment_scripts(&ins, std::slice::from_ref(&plain));
        let (t, p) = tx(
            &ins,
            vec![
                (50_000, sp[0].clone()),
                (40_000, p2wpkh_script(&sender_key(0))),
                (9_000, unrelated_p2tr(0)),
            ],
        );
        plan.push(vec![(
            t,
            p,
            vec![(0, 0, None, "single payment, P2WPKH input")],
        )]);
    }

    // Height 3: two inputs of different types, two outputs for the same
    // receiver (k = 0 and k = 1) at non-zero positions (T03, T04); plus a
    // payment to somebody else.
    {
        let ins = [
            input(1, InputKind::P2trKeyPath),
            input(2, InputKind::P2wpkh),
        ];
        let sp = silent_payment_scripts(&ins, &[plain.clone(), plain.clone()]);
        let (t1, p1) = tx(
            &ins,
            vec![
                (150_000, p2wpkh_script(&sender_key(1))),
                (10_000, sp[0].clone()),
                (20_000, sp[1].clone()),
            ],
        );
        let ins2 = [input(3, InputKind::P2wpkh)];
        let sp2 = silent_payment_scripts(&ins2, &[code(other.address(network))]);
        let (t2, p2) = tx(&ins2, vec![(90_000, sp2[0].clone())]);
        plan.push(vec![
            (
                t1,
                p1,
                vec![
                    (
                        1,
                        0,
                        None,
                        "two inputs (P2TR + P2WPKH), first of two outputs",
                    ),
                    (
                        2,
                        1,
                        None,
                        "two inputs (P2TR + P2WPKH), second of two outputs",
                    ),
                ],
            ),
            (t2, p2, vec![]),
        ]);
    }

    // Height 4: coinbase only.
    plan.push(vec![]);

    // Height 5: labels (T05) — a used label (k = 0), the change label (k = 1)
    // and a label the receiver did not back up (k = 2), all in one
    // transaction. A wallet that does not know the label at k = 0 cannot find
    // the later outputs either, not even the change output.
    {
        let ins = [input(4, InputKind::P2trKeyPath)];
        let sp = silent_payment_scripts(
            &ins,
            &[label_used.clone(), label_change.clone(), label_lost.clone()],
        );
        let (t, p) = tx(
            &ins,
            vec![
                (7_000, sp[0].clone()),
                (3_000, sp[1].clone()),
                (1_000, sp[2].clone()),
            ],
        );
        plan.push(vec![(
            t,
            p,
            vec![
                (0, 0, Some(LABEL_USED), "labelled address"),
                (
                    1,
                    1,
                    Some(0),
                    "change label (m = 0) behind a labelled output",
                ),
                (
                    2,
                    2,
                    Some(LABEL_NOT_BACKED_UP),
                    "label the wallet may not know after restore",
                ),
            ],
        )]);
    }

    if !fork {
        // Height 6: transactions BIP352 says must not be scanned.
        {
            // No eligible input (P2WSH only).
            let ins_a = [input(5, InputKind::P2wsh)];
            let (ta, pa) = tx(&ins_a, vec![(90_000, unrelated_p2tr(1))]);

            // Spends a SegWit v2 output next to an eligible input. The output is
            // what a non-compliant sender would derive for the receiver; BIP352
            // receivers must not scan this transaction, so it is NOT expected.
            let ins_b = [input(6, InputKind::P2wpkh), input(7, InputKind::SegwitV2)];
            let sp_b = silent_payment_scripts(&ins_b, std::slice::from_ref(&plain));
            let (tb, pb) = tx(&ins_b, vec![(60_000, sp_b[0].clone())]);

            // Eligible input but no taproot output.
            let ins_c = [input(8, InputKind::P2wpkh)];
            let (tc, pc) = tx(&ins_c, vec![(90_000, p2wpkh_script(&sender_key(8)))]);

            plan.push(vec![(ta, pa, vec![]), (tb, pb, vec![]), (tc, pc, vec![])]);
        }

        // Height 7: a payment to the change label only (k = 0). Found by any
        // wallet that has the keys, because the change label is always scanned.
        {
            let ins = [input(10, InputKind::P2wpkh)];
            let sp = silent_payment_scripts(&ins, std::slice::from_ref(&label_change));
            let (t, p) = tx(
                &ins,
                vec![(80_000, unrelated_p2tr(3)), (15_000, sp[0].clone())],
            );
            plan.push(vec![(
                t,
                p,
                vec![(1, 0, Some(0), "change label (m = 0) alone")],
            )]);
        }

        // Height 8: a dust-sized payment. Detection is cryptographic; value must
        // never be a reason to ignore an output.
        {
            let ins = [input(9, InputKind::P2wpkh)];
            let sp = silent_payment_scripts(&ins, std::slice::from_ref(&plain));
            let (t, p) = tx(&ins, vec![(1, sp[0].clone()), (90_000, unrelated_p2tr(2))]);
            plan.push(vec![(t, p, vec![(0, 0, None, "1 sat payment")])]);
        }
    } else {
        // Height 6': a payment that only exists on the fork.
        {
            let ins = [input(20, InputKind::P2wpkh)];
            let sp = silent_payment_scripts(&ins, std::slice::from_ref(&plain));
            let (t, p) = tx(&ins, vec![(33_000, sp[0].clone())]);
            plan.push(vec![(t, p, vec![(0, 0, None, "payment only on the fork")])]);
        }
        // Height 7': coinbase only.
        plan.push(vec![]);
        // Height 8': the main chain's 1 sat transaction, confirmed here too.
        {
            let ins = [input(9, InputKind::P2wpkh)];
            let sp = silent_payment_scripts(&ins, std::slice::from_ref(&plain));
            let (t, p) = tx(&ins, vec![(1, sp[0].clone()), (90_000, unrelated_p2tr(2))]);
            plan.push(vec![(
                t,
                p,
                vec![(
                    0,
                    0,
                    None,
                    "1 sat payment (same transaction as on the main chain)",
                )],
            )]);
        }
        // Height 9': coinbase only.
        plan.push(vec![]);
    }

    for (index, planned) in plan.into_iter().enumerate() {
        let height = u32::try_from(index + 1).unwrap();
        let mut txs = Vec::new();
        let mut prevouts = vec![vec![]]; // coinbase
        for (t, p, created) in planned {
            let txid = t.compute_txid();
            for (vout, k, label, note) in created {
                expected.push(FixtureExpectedDetection {
                    height,
                    txid,
                    vout,
                    k,
                    value_sats: t.output[vout as usize].value.to_sat(),
                    label,
                    note: note.to_string(),
                });
            }
            txs.push(t);
            prevouts.push(p);
        }
        let b = block(height, prev, txs);
        prev = b.block_hash();
        blocks.push(FixtureBlock {
            height,
            block_hex: serialize_hex(&b),
            prevouts,
        });
    }

    FixtureFile {
        format: FIXTURE_FORMAT.to_string(),
        description: if fork {
            "Fork of the deterministic fixture chain (identical up to height 5), for reorg \
             tests. Generated in memory by lab-testkit; not stored in the repository."
                .to_string()
        } else {
            MAIN_DESCRIPTION.to_string()
        },
        network,
        genesis_hash: network.genesis_hash(),
        blocks,
        expected_detections: expected,
    }
}

/// Shape of a chain made by [`generate_long`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongChain {
    /// Number of blocks (heights `1..=blocks`).
    pub blocks: u32,
    /// Non-coinbase transactions per block.
    pub txs_per_block: u32,
    /// Every `payment_every`-th block carries one payment to the lab receiver.
    pub payment_every: u32,
}

impl LongChain {
    /// The shape used for benchmarks and scale tests: 4 transactions per
    /// block, one payment to the receiver every 10 blocks.
    pub fn standard(blocks: u32) -> Self {
        Self {
            blocks,
            txs_per_block: 4,
            payment_every: 10,
        }
    }
}

/// A deterministic chain of any length, for scale tests and benchmarks.
///
/// Each block holds `txs_per_block` transactions that cycle through the cases
/// a scanner meets in practice: a payment to another receiver (scanned, no
/// match), a taproot key-path spend to an unrelated taproot output (scanned,
/// no match), a transaction without a taproot output (not scanned) and one
/// with only a non-eligible input (not scanned). In every `payment_every`-th
/// block the first transaction pays the lab receiver instead: alternately its
/// plain address and its label-1 address.
///
/// Like [`generate`], transactions carry placeholder signatures and are not
/// consensus-valid. Nothing here is stored in the repository.
pub fn generate_long(shape: &LongChain) -> FixtureFile {
    assert!(shape.blocks >= 1 && shape.payment_every >= 1);
    let network = LabNetwork::Regtest;
    let receiver = lab_receiver_keys();
    let other = other_receiver_keys();
    let code = |a: LabAddress| {
        SilentPaymentCode::new_v0(a.scan_public_key(), a.spend_public_key(), Network::Regtest)
    };
    let plain = code(receiver.address(network));
    let label_used = code(receiver.labelled_address(network, LABEL_USED).unwrap());
    let other_code = code(other.address(network));

    let mut blocks = Vec::with_capacity(shape.blocks as usize);
    let mut expected = Vec::new();
    let mut prev = network.genesis_hash();
    // Input indices far away from the ones the basic fixture uses.
    let mut next_input = 1_000_000u32;
    let mut take = || {
        next_input += 1;
        next_input
    };

    for height in 1..=shape.blocks {
        let mut txs = Vec::new();
        let mut prevouts = vec![vec![]]; // coinbase
        for slot in 0..shape.txs_per_block {
            let pays_receiver = slot == 0 && height % shape.payment_every == 0;
            let (t, p) = if pays_receiver {
                let nth = height / shape.payment_every;
                let ins = [input(take(), InputKind::P2wpkh)];
                let (target, label) = if nth % 2 == 0 {
                    (&label_used, Some(LABEL_USED))
                } else {
                    (&plain, None)
                };
                let sp = silent_payment_scripts(&ins, std::slice::from_ref(target));
                let value = 10_000 + u64::from(nth);
                let (t, p) = tx(
                    &ins,
                    vec![
                        (80_000, p2wpkh_script(&sender_key(height))),
                        (value, sp[0].clone()),
                    ],
                );
                expected.push(FixtureExpectedDetection {
                    height,
                    txid: t.compute_txid(),
                    vout: 1,
                    k: 0,
                    value_sats: value,
                    label,
                    note: format!("long chain payment #{nth}"),
                });
                (t, p)
            } else {
                match (height + slot) % 4 {
                    0 => {
                        let ins = [input(take(), InputKind::P2wpkh)];
                        let sp = silent_payment_scripts(&ins, std::slice::from_ref(&other_code));
                        tx(&ins, vec![(90_000, sp[0].clone())])
                    }
                    1 => {
                        let ins = [input(take(), InputKind::P2trKeyPath)];
                        tx(&ins, vec![(90_000, unrelated_p2tr(height * 8 + slot))])
                    }
                    2 => {
                        let ins = [input(take(), InputKind::P2wpkh)];
                        tx(&ins, vec![(90_000, p2wpkh_script(&sender_key(height)))])
                    }
                    _ => {
                        let ins = [input(take(), InputKind::P2wsh)];
                        tx(&ins, vec![(90_000, unrelated_p2tr(height * 8 + slot))])
                    }
                }
            };
            txs.push(t);
            prevouts.push(p);
        }
        let b = block(height, prev, txs);
        prev = b.block_hash();
        blocks.push(FixtureBlock {
            height,
            block_hex: serialize_hex(&b),
            prevouts,
        });
    }

    FixtureFile {
        format: FIXTURE_FORMAT.to_string(),
        description: format!(
            "Deterministic long chain for scale tests and benchmarks ({} blocks, {} transactions \
             per block, one payment to the lab receiver every {} blocks). Generated by \
             lab-testkit; placeholder signatures, NOT consensus-valid; not stored in the \
             repository.",
            shape.blocks, shape.txs_per_block, shape.payment_every
        ),
        network,
        genesis_hash: network.genesis_hash(),
        blocks,
        expected_detections: expected,
    }
}

const MAIN_DESCRIPTION: &str = "Deterministic regtest-shaped chain for Silent Sync Lab Phase 1. \
                      Receiver = BIP352 test vector #0 keys. Transactions carry placeholder \
                      signatures and are NOT consensus-valid. `expected_detections` is \
                      sender-side knowledge of outputs created for the receiver. Regenerate \
                      with `cargo run -p lab-testkit --bin gen_fixture`.";

/// Canonical on-disk form.
pub fn generate_json() -> String {
    let mut json = serde_json::to_string_pretty(&generate()).unwrap();
    json.push('\n');
    json
}
