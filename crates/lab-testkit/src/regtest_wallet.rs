//! Sender side of the end-to-end tests: funds throwaway keys from the regtest
//! node's wallet and builds **really signed** silent payment transactions.
//!
//! TEST-DATA GENERATION ONLY. Signing happens here, with `rust-bitcoin`'s
//! sighash and `secp256k1` signing functions, and silent payment outputs are
//! derived with upstream `bdk_sp` sending functions. None of this is reachable
//! from `sync-core` or the CLI.
//!
//! Sender keys are derived from fixed public strings; they only ever control
//! regtest coins on a node that is deleted after the test.

use crate::{
    fixture_gen::{self, LABEL_NOT_BACKED_UP, LABEL_USED},
    regtest_node::Node,
};
use bdk_sp::{
    encoding::SilentPaymentCode,
    send::{create_silentpayment_partial_secret, create_silentpayment_scriptpubkeys},
    LexMin,
};
use sync_core::{
    bitcoin::{
        absolute::LockTime,
        consensus::encode::{deserialize_hex, serialize_hex},
        ecdsa,
        hashes::{sha256, Hash},
        key::{CompressedPublicKey, Keypair, Secp256k1, TapTweak},
        opcodes::all::OP_PUSHNUM_1,
        script::Builder,
        secp256k1::{Message, SecretKey},
        sighash::{EcdsaSighashType, Prevouts, SighashCache, TapSighashType},
        taproot,
        transaction::Version,
        Address, Amount, Denomination, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn,
        TxOut, Witness, WitnessProgram, WitnessVersion,
    },
    network::LabNetwork,
    protocol::LabAddress,
    source::fixture::FixtureExpectedDetection,
};

const FUNDING_SATS: u64 = 100_000;
const WALLET_NAME: &str = "lab-testkit";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// Eligible for shared secret derivation.
    P2wpkh,
    /// Eligible: taproot key-path spend.
    P2trKeyPath,
    /// Not eligible: P2WSH of `OP_TRUE`.
    P2wshTrue,
    /// SegWit v2 output (anyone-can-spend by consensus); a transaction
    /// spending it must not be scanned.
    SegwitV2,
}

/// A confirmed output on the regtest chain that this module can spend.
#[derive(Debug, Clone)]
pub struct Funded {
    pub kind: InputKind,
    pub outpoint: OutPoint,
    pub prevout: TxOut,
    key: SecretKey,
}

fn sender_key(i: u32) -> SecretKey {
    let digest = sha256::Hash::hash(format!("silent-sync-lab/regtest/sender-key/{i}").as_bytes());
    SecretKey::from_slice(digest.as_byte_array()).expect("valid key")
}

fn witness_script_true() -> ScriptBuf {
    Builder::new().push_opcode(OP_PUSHNUM_1).into_script()
}

fn script_for(kind: InputKind, key: &SecretKey, i: u32) -> ScriptBuf {
    let secp = Secp256k1::new();
    match kind {
        InputKind::P2wpkh => {
            ScriptBuf::new_p2wpkh(&CompressedPublicKey(key.public_key(&secp)).wpubkey_hash())
        }
        InputKind::P2trKeyPath => {
            let (internal, _) = Keypair::from_secret_key(&secp, key).x_only_public_key();
            ScriptBuf::new_p2tr(&secp, internal, None)
        }
        InputKind::P2wshTrue => ScriptBuf::new_p2wsh(&witness_script_true().wscript_hash()),
        InputKind::SegwitV2 => {
            let program = sha256::Hash::hash(format!("silent-sync-lab/regtest/v2/{i}").as_bytes());
            ScriptBuf::new_witness_program(
                &WitnessProgram::new(WitnessVersion::V2, program.as_byte_array()).expect("program"),
            )
        }
    }
}

/// The private key BIP352 senders use for an input, if the input is eligible.
fn shared_secret_key(input: &Funded) -> Option<SecretKey> {
    let secp = Secp256k1::new();
    match input.kind {
        InputKind::P2wpkh => Some(input.key),
        // The key of the taproot *output* key, i.e. after the BIP341 tweak.
        InputKind::P2trKeyPath => Some(
            Keypair::from_secret_key(&secp, &input.key)
                .tap_tweak(&secp, None)
                .to_keypair()
                .secret_key(),
        ),
        InputKind::P2wshTrue | InputKind::SegwitV2 => None,
    }
}

/// Upstream's address type for a lab address (needed by the sending functions).
pub fn upstream_code(address: LabAddress) -> SilentPaymentCode {
    SilentPaymentCode::new_v0(
        address.scan_public_key(),
        address.spend_public_key(),
        Network::Regtest,
    )
}

/// Silent payment output scripts for `recipients`, in order, as a BIP352
/// sender spending `inputs` derives them.
pub fn silent_payment_scripts(inputs: &[Funded], recipients: &[LabAddress]) -> Vec<ScriptBuf> {
    let mut lex_min = LexMin::default();
    let mut spks_with_keys = Vec::new();
    for input in inputs {
        lex_min.update(&input.outpoint);
        if let Some(key) = shared_secret_key(input) {
            spks_with_keys.push((input.prevout.script_pubkey.clone(), key));
        }
    }
    let partial =
        create_silentpayment_partial_secret(&lex_min.bytes().expect("inputs"), &spks_with_keys)
            .expect("at least one eligible input");
    let codes: Vec<SilentPaymentCode> = recipients.iter().copied().map(upstream_code).collect();
    let mut by_code = create_silentpayment_scriptpubkeys(partial, &codes);
    codes
        .iter()
        .map(|code| {
            let key = by_code.get_mut(code).expect("recipient").remove(0);
            ScriptBuf::new_p2tr_tweaked(
                sync_core::bitcoin::key::TweakedPublicKey::dangerous_assume_tweaked(key),
            )
        })
        .collect()
}

/// Builds a transaction spending `inputs` to `outputs` and signs every input.
pub fn signed_transaction(inputs: &[Funded], outputs: Vec<(u64, ScriptBuf)>) -> Transaction {
    let secp = Secp256k1::new();
    let mut tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: inputs
            .iter()
            .map(|i| TxIn {
                previous_output: i.outpoint,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: outputs
            .into_iter()
            .map(|(sats, script_pubkey)| TxOut {
                value: Amount::from_sat(sats),
                script_pubkey,
            })
            .collect(),
    };
    let prevouts: Vec<TxOut> = inputs.iter().map(|i| i.prevout.clone()).collect();

    let mut witnesses = Vec::new();
    {
        let mut cache = SighashCache::new(&tx);
        for (index, input) in inputs.iter().enumerate() {
            let witness = match input.kind {
                InputKind::P2wpkh => {
                    let sighash = cache
                        .p2wpkh_signature_hash(
                            index,
                            &input.prevout.script_pubkey,
                            input.prevout.value,
                            EcdsaSighashType::All,
                        )
                        .expect("sighash");
                    let signature = ecdsa::Signature {
                        signature: secp.sign_ecdsa(&Message::from(sighash), &input.key),
                        sighash_type: EcdsaSighashType::All,
                    };
                    Witness::p2wpkh(&signature, &input.key.public_key(&secp))
                }
                InputKind::P2trKeyPath => {
                    let sighash = cache
                        .taproot_key_spend_signature_hash(
                            index,
                            &Prevouts::All(&prevouts),
                            TapSighashType::Default,
                        )
                        .expect("sighash");
                    let tweaked =
                        Keypair::from_secret_key(&secp, &input.key).tap_tweak(&secp, None);
                    let signature = taproot::Signature {
                        signature: secp.sign_schnorr_no_aux_rand(
                            &Message::from(sighash),
                            &tweaked.to_keypair(),
                        ),
                        sighash_type: TapSighashType::Default,
                    };
                    Witness::p2tr_key_spend(&signature)
                }
                InputKind::P2wshTrue => Witness::from_slice(&[witness_script_true().to_bytes()]),
                // Unknown witness versions succeed unconditionally by consensus.
                InputKind::SegwitV2 => Witness::new(),
            };
            witnesses.push(witness);
        }
    }
    for (txin, witness) in tx.input.iter_mut().zip(witnesses) {
        txin.witness = witness;
    }
    tx
}

/// The node's wallet, used only to hand out regtest coins.
pub struct Faucet<'a> {
    node: &'a Node,
    miner_address: String,
    next_key: u32,
}

impl<'a> Faucet<'a> {
    /// Creates a wallet on the node and mines enough blocks for spendable coins.
    pub fn new(node: &'a Node) -> Self {
        node.cli(&["createwallet", WALLET_NAME]);
        let miner_address = node.cli(&["getnewaddress", "", "bech32"]);
        node.generate_to(101, &miner_address);
        Self {
            node,
            miner_address,
            next_key: 0,
        }
    }

    pub fn miner_address(&self) -> &str {
        &self.miner_address
    }

    /// Sends coins to fresh test keys of the given kinds and confirms them in
    /// one block.
    pub fn fund(&mut self, kinds: &[InputKind]) -> Vec<Funded> {
        let amount = Amount::from_sat(FUNDING_SATS).to_string_in(Denomination::Bitcoin);
        let mut pending = Vec::new();
        for &kind in kinds {
            let index = self.next_key;
            self.next_key += 1;
            let key = sender_key(index);
            let script = script_for(kind, &key, index);
            let address = Address::from_script(&script, Network::Regtest).expect("address");
            let txid = self
                .node
                .cli(&["sendtoaddress", &address.to_string(), &amount]);
            pending.push((kind, key, script, txid));
        }
        self.node.generate_to(1, &self.miner_address);

        pending
            .into_iter()
            .map(|(kind, key, script, txid)| {
                let info = self.node.cli_json(&["gettransaction", &txid]);
                let tx: Transaction =
                    deserialize_hex(info["hex"].as_str().expect("hex")).expect("transaction");
                let vout = tx
                    .output
                    .iter()
                    .position(|o| o.script_pubkey == script)
                    .expect("funding output");
                Funded {
                    kind,
                    outpoint: OutPoint::new(tx.compute_txid(), vout as u32),
                    prevout: tx.output[vout].clone(),
                    key,
                }
            })
            .collect()
    }

    /// Mines one block with exactly these transactions (mempool policy is
    /// bypassed, consensus is not) and returns its height.
    pub fn mine_block(&self, txs: &[&Transaction]) -> u32 {
        let raw: Vec<String> = txs.iter().map(|tx| serialize_hex(*tx)).collect();
        self.node.generate_block_with(&self.miner_address, &raw);
        self.node.block_count()
    }

    /// Broadcasts through the mempool (policy applies) and mines the next
    /// block; returns its height.
    pub fn broadcast_and_mine(&self, tx: &Transaction) -> u32 {
        let txid = self.node.cli(&["sendrawtransaction", &serialize_hex(tx)]);
        assert_eq!(txid, tx.compute_txid().to_string());
        self.node.generate_to(1, &self.miner_address);
        self.node.block_count()
    }
}

/// What the standard scenario put on chain.
#[derive(Debug, Clone)]
pub struct Scenario {
    /// Height of the last block mined before the scenario's payment blocks.
    pub base_height: u32,
    /// Height of the last scenario block.
    pub tip_height: u32,
    /// Sender-side knowledge: outputs created for the lab receiver.
    pub expected: Vec<FixtureExpectedDetection>,
    /// Txid of the transaction that spends a SegWit v2 output and carries an
    /// output a non-compliant receiver would attribute to the lab receiver.
    pub segwit_v2_txid: sync_core::bitcoin::Txid,
    /// The change-label-only payment (second-to-last block) and the 1 sat
    /// payment (last block), kept so reorg tests can confirm them again.
    pub last_two_payments: [Transaction; 2],
}

fn change_script(i: u32) -> ScriptBuf {
    script_for(InputKind::P2wpkh, &sender_key(10_000 + i), 0)
}

fn unrelated_p2tr(i: u32) -> ScriptBuf {
    script_for(InputKind::P2trKeyPath, &sender_key(20_000 + i), 0)
}

/// Puts the same situations as the offline fixture on a real regtest chain,
/// with consensus-valid, signed transactions:
///
/// 1. one payment (through the mempool);
/// 2. two inputs of different types paying two outputs, plus a payment to
///    another receiver;
/// 3. label 1 (k=0), change label (k=1) and label 5 (k=2) in one transaction;
/// 4. transactions BIP352 says must not be scanned (P2WSH only / spends
///    SegWit v2 / no taproot output);
/// 5. a change-label-only payment;
/// 6. a 1 sat payment.
pub fn standard_scenario(faucet: &mut Faucet<'_>) -> Scenario {
    use InputKind::*;
    let network = LabNetwork::Regtest;
    let receiver = fixture_gen::lab_receiver_keys();
    let other = fixture_gen::other_receiver_keys();
    let plain = receiver.address(network);
    let label_used = receiver
        .labelled_address(network, LABEL_USED)
        .expect("label");
    let label_change = receiver.labelled_address(network, 0).expect("label");
    let label_lost = receiver
        .labelled_address(network, LABEL_NOT_BACKED_UP)
        .expect("label");

    let coins = faucet.fund(&[
        P2wpkh,      // 0: block 1
        P2trKeyPath, // 1: block 2
        P2wpkh,      // 2: block 2
        P2wpkh,      // 3: block 2 (other receiver)
        P2trKeyPath, // 4: block 3
        P2wshTrue,   // 5: block 4
        P2wpkh,      // 6: block 4 (with SegWit v2)
        SegwitV2,    // 7: block 4
        P2wpkh,      // 8: block 4 (no taproot output)
        P2wpkh,      // 9: block 5
        P2wpkh,      // 10: block 6
    ]);
    let base_height = faucet.node.block_count();
    let mut expected = Vec::new();
    let mut expect = |height: u32, tx: &Transaction, entries: &[(u32, u32, Option<u32>, &str)]| {
        for &(vout, k, label, note) in entries {
            expected.push(FixtureExpectedDetection {
                height,
                txid: tx.compute_txid(),
                vout,
                k,
                value_sats: tx.output[vout as usize].value.to_sat(),
                label,
                note: note.to_string(),
            });
        }
    };

    // Block 1: single payment, relayed through the mempool.
    let ins = &coins[0..1];
    let sp = silent_payment_scripts(ins, &[plain]);
    let tx = signed_transaction(
        ins,
        vec![
            (50_000, sp[0].clone()),
            (40_000, change_script(0)),
            (9_000, unrelated_p2tr(0)),
        ],
    );
    let height = faucet.broadcast_and_mine(&tx);
    expect(height, &tx, &[(0, 0, None, "single payment, P2WPKH input")]);

    // Block 2: P2TR + P2WPKH inputs, two outputs; and someone else's payment.
    let ins = &coins[1..3];
    let sp = silent_payment_scripts(ins, &[plain, plain]);
    let tx_two = signed_transaction(
        ins,
        vec![
            (150_000, change_script(1)),
            (10_000, sp[0].clone()),
            (20_000, sp[1].clone()),
        ],
    );
    let ins_other = &coins[3..4];
    let sp_other = silent_payment_scripts(ins_other, &[other.address(network)]);
    let tx_other = signed_transaction(ins_other, vec![(90_000, sp_other[0].clone())]);
    let height = faucet.mine_block(&[&tx_two, &tx_other]);
    expect(
        height,
        &tx_two,
        &[
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
    );

    // Block 3: labels.
    let ins = &coins[4..5];
    let sp = silent_payment_scripts(ins, &[label_used, label_change, label_lost]);
    let tx = signed_transaction(
        ins,
        vec![
            (7_000, sp[0].clone()),
            (3_000, sp[1].clone()),
            (1_000, sp[2].clone()),
            (80_000, change_script(2)),
        ],
    );
    let height = faucet.mine_block(&[&tx]);
    expect(
        height,
        &tx,
        &[
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
    );

    // Block 4: not scannable.
    let tx_wsh = signed_transaction(&coins[5..6], vec![(90_000, unrelated_p2tr(1))]);
    let ins_v2 = &coins[6..8];
    let sp_v2 = silent_payment_scripts(ins_v2, &[plain]);
    let tx_v2 = signed_transaction(ins_v2, vec![(60_000, sp_v2[0].clone())]);
    let tx_no_taproot = signed_transaction(&coins[8..9], vec![(90_000, change_script(3))]);
    faucet.mine_block(&[&tx_wsh, &tx_v2, &tx_no_taproot]);

    // Block 5: change label alone.
    let ins = &coins[9..10];
    let sp = silent_payment_scripts(ins, &[label_change]);
    let tx = signed_transaction(
        ins,
        vec![(80_000, unrelated_p2tr(3)), (15_000, sp[0].clone())],
    );
    let height = faucet.mine_block(&[&tx]);
    expect(
        height,
        &tx,
        &[(1, 0, Some(0), "change label (m = 0) alone")],
    );
    let change_only = tx;

    // Block 6: 1 sat.
    let ins = &coins[10..11];
    let sp = silent_payment_scripts(ins, &[plain]);
    let tx = signed_transaction(ins, vec![(1, sp[0].clone()), (90_000, unrelated_p2tr(2))]);
    let height = faucet.mine_block(&[&tx]);
    expect(height, &tx, &[(0, 0, None, "1 sat payment")]);

    Scenario {
        base_height,
        tip_height: height,
        expected,
        segwit_v2_txid: tx_v2.compute_txid(),
        last_two_payments: [change_only, tx],
    }
}

/// What a wallet knowing `labels` (plus the change label, if listed) must
/// find, from sender-side knowledge only: per transaction, outputs are found
/// in `k` order until the first one whose label the wallet does not know.
pub fn expected_for_labels(
    expected: &[FixtureExpectedDetection],
    labels: &[u32],
) -> Vec<(u32, sync_core::bitcoin::Txid, u32, u64, Option<u32>)> {
    use std::collections::BTreeMap;
    let mut by_tx: BTreeMap<_, Vec<&FixtureExpectedDetection>> = BTreeMap::new();
    for e in expected {
        by_tx.entry((e.height, e.txid)).or_default().push(e);
    }
    let mut found = Vec::new();
    for (_, mut outputs) in by_tx {
        outputs.sort_by_key(|e| e.k);
        found.extend(
            outputs
                .into_iter()
                .take_while(|e| e.label.is_none_or(|m| labels.contains(&m)))
                .map(|e| (e.height, e.txid, e.vout, e.value_sats, e.label)),
        );
    }
    found.sort_by_key(|e| (e.0, e.2));
    found
}
