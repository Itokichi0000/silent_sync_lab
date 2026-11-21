//! T01 — BIP352 official receiving test vectors, run through `sync-core`'s
//! `ReceiveScanner` (which delegates all cryptography to upstream `bdk_sp`).
//!
//! The vector file is the canonical one from bitcoin/bips, pinned by SHA-256
//! (see `fixtures/bip352/SOURCE.md`). Only the *receiving* halves are checked:
//! the lab is receive-only; sending is exercised by upstream's own test suite.
//!
//! All private keys in this file come from that public vector file.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use serde::Deserialize;
use std::collections::BTreeSet;
use sync_core::{
    bitcoin::{
        absolute::LockTime,
        consensus::deserialize,
        hashes::{sha256, Hash},
        hex::FromHex,
        key::Secp256k1,
        secp256k1::{Message, PublicKey, Scalar, SecretKey},
        transaction::Version,
        Amount, Network, OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness,
    },
    network::LabNetwork,
    protocol::{
        LabAddress, ProtocolError, ReceiveScanner, ReceiverKeys, SkipReason, TxScanOutcome, K_MAX,
    },
};

const VECTORS_JSON: &str =
    include_str!("../../../fixtures/bip352/send_and_receive_test_vectors.json");

/// SHA-256 of the canonical file at bitcoin/bips commit
/// fdcd8024595bc1e97db73883e716dbb91627c371 (BIP352 v1.1.2).
const VECTORS_SHA256: &str = "23cc7c3b496557970b34c0d820d412260ee1cc1f6046b2d5c0c7a016de88706c";
const VECTOR_COUNT: usize = 29;
const K_MAX_VECTOR: &str = "Maximum per-group recipient limit K_max is exceeded (2324 matches): \
                            sending fails, receiver doesn't scan beyond limit";

#[derive(Deserialize)]
struct TestCase {
    comment: String,
    receiving: Vec<Receiving>,
}

#[derive(Deserialize)]
struct Receiving {
    given: Given,
    expected: Expected,
}

#[derive(Deserialize)]
struct Given {
    vin: Vec<Vin>,
    outputs: Vec<String>,
    key_material: KeyMaterial,
    labels: Vec<u32>,
}

#[derive(Deserialize)]
struct Vin {
    txid: Txid,
    vout: u32,
    #[serde(rename = "scriptSig")]
    script_sig: String,
    txinwitness: String,
    prevout: Prevout,
}

#[derive(Deserialize)]
struct Prevout {
    #[serde(rename = "scriptPubKey")]
    script_pub_key: ScriptPubKey,
}

#[derive(Deserialize)]
struct ScriptPubKey {
    hex: String,
}

#[derive(Deserialize)]
struct KeyMaterial {
    spend_priv_key: SecretKey,
    scan_priv_key: SecretKey,
}

#[derive(Deserialize)]
struct Expected {
    addresses: Vec<String>,
    /// Present for every case except the K_max one, which gives `n_outputs`.
    outputs: Option<Vec<ExpectedOutput>>,
    n_outputs: Option<usize>,
    /// BIP352 public tweak data (`input_hash·A`); `null` when not eligible.
    tweak: Option<PublicKey>,
}

#[derive(Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ExpectedOutput {
    priv_key_tweak: String,
    pub_key: String,
    signature: String,
}

fn vectors() -> Vec<TestCase> {
    serde_json::from_str(VECTORS_JSON).unwrap()
}

fn build_tx(given: &Given) -> (Transaction, Vec<TxOut>) {
    let mut input = Vec::new();
    let mut prevouts = Vec::new();
    for vin in &given.vin {
        let witness = if vin.txinwitness.is_empty() {
            Witness::new()
        } else {
            deserialize::<Witness>(&Vec::<u8>::from_hex(&vin.txinwitness).unwrap()).unwrap()
        };
        input.push(TxIn {
            previous_output: OutPoint::new(vin.txid, vin.vout),
            script_sig: ScriptBuf::from_hex(&vin.script_sig).unwrap(),
            sequence: Sequence::MAX,
            witness,
        });
        prevouts.push(TxOut {
            value: Amount::ZERO,
            script_pubkey: ScriptBuf::from_hex(&vin.prevout.script_pub_key.hex).unwrap(),
        });
    }
    let output = given
        .outputs
        .iter()
        .map(|x| {
            // Built from raw bytes on purpose: some vectors contain taproot
            // outputs whose 32 bytes are not a valid x-only key, exactly as
            // can happen on chain. The scanner must cope with those.
            let program = <[u8; 32]>::from_hex(x).unwrap();
            let mut script = vec![0x51, 0x20];
            script.extend_from_slice(&program);
            let script_pubkey = ScriptBuf::from_bytes(script);
            assert!(script_pubkey.is_p2tr());
            TxOut {
                value: Amount::ZERO,
                script_pubkey,
            }
        })
        .collect();
    let tx = Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input,
        output,
    };
    (tx, prevouts)
}

fn receiver(given: &Given) -> (ReceiverKeys, SecretKey) {
    let secp = Secp256k1::new();
    let spend_sk = given.key_material.spend_priv_key;
    let keys = ReceiverKeys::new(given.key_material.scan_priv_key, spend_sk.public_key(&secp));
    (keys, spend_sk)
}

/// Addresses in the vectors use the mainnet prefix. `sync-core` cannot encode
/// for mainnet, so the keys it derived are re-encoded here with upstream's
/// encoder purely to compare strings.
fn mainnet_string(address: &LabAddress) -> String {
    bdk_sp::encoding::SilentPaymentCode::new_v0(
        address.scan_public_key(),
        address.spend_public_key(),
        Network::Bitcoin,
    )
    .to_string()
}

/// Checks everything about one receiving case except the K_max special case.
fn check_receiving(comment: &str, case: &Receiving) {
    let secp = Secp256k1::new();
    let (keys, spend_sk) = receiver(&case.given);
    let (tx, prevouts) = build_tx(&case.given);

    // Address derivation, including labelled addresses.
    let mut addresses = BTreeSet::new();
    addresses.insert(mainnet_string(&keys.address(LabNetwork::Regtest)));
    for &m in &case.given.labels {
        addresses.insert(mainnet_string(
            &keys.labelled_address(LabNetwork::Regtest, m).unwrap(),
        ));
    }
    let expected_addresses: BTreeSet<String> = case.expected.addresses.iter().cloned().collect();
    assert_eq!(addresses, expected_addresses, "{comment}: addresses");

    // Public tweak data, derived without any private key.
    let tweak = ReceiveScanner::public_tweak_data(&tx, &prevouts).unwrap();
    assert_eq!(tweak.ok(), case.expected.tweak, "{comment}: tweak data");

    // Detection.
    let scanner = ReceiveScanner::with_exact_labels(&keys, &case.given.labels).unwrap();
    let matches = match scanner.scan_tx(&tx, &prevouts).unwrap() {
        TxScanOutcome::Scanned { matches } => matches,
        TxScanOutcome::NotEligible(reason) => {
            assert!(
                case.expected.tweak.is_none(),
                "{comment}: skipped ({reason:?}) but eligible"
            );
            Vec::new()
        }
    };

    let msg = Message::from_digest(sha256::Hash::hash(b"message").to_byte_array());
    let aux = sha256::Hash::hash(b"random auxiliary data").to_byte_array();
    let mut found: Vec<ExpectedOutput> = matches
        .iter()
        .map(|m| {
            // The vectors publish spend private keys, so the full output key
            // can be reconstructed and the reference signature reproduced.
            let output_sk = spend_sk
                .add_tweak(&Scalar::from(m.sensitive_output_tweak()))
                .unwrap();
            let (output_pk, _) = output_sk.x_only_public_key(&secp);
            assert_eq!(
                m.script_pubkey.as_bytes()[2..],
                output_pk.serialize(),
                "{comment}: tweak does not open the matched output"
            );
            let sig = secp.sign_schnorr_with_aux_rand(&msg, &output_sk.keypair(&secp), &aux);
            ExpectedOutput {
                priv_key_tweak: m.sensitive_output_tweak().display_secret().to_string(),
                pub_key: output_pk.to_string(),
                signature: sig.to_string(),
            }
        })
        .collect();
    found.sort();

    let mut expected: Vec<ExpectedOutput> = case
        .expected
        .outputs
        .as_ref()
        .unwrap_or_else(|| panic!("{comment}: no `outputs` in expected"))
        .iter()
        .map(|o| ExpectedOutput {
            priv_key_tweak: o.priv_key_tweak.clone(),
            pub_key: o.pub_key.clone(),
            signature: o.signature.clone(),
        })
        .collect();
    expected.sort();
    assert_eq!(found, expected, "{comment}: detected outputs");
}

#[test]
fn vector_file_is_the_pinned_canonical_file() {
    let digest = sha256::Hash::hash(VECTORS_JSON.as_bytes());
    assert_eq!(digest.to_string(), VECTORS_SHA256);
    let all = vectors();
    assert_eq!(all.len(), VECTOR_COUNT);
    assert_eq!(all.iter().filter(|c| c.comment == K_MAX_VECTOR).count(), 1);
}

#[test]
fn all_receiving_vectors_except_k_max() {
    let all = vectors();
    let mut cases = 0;
    for case in all.iter().filter(|c| c.comment != K_MAX_VECTOR) {
        assert!(
            !case.receiving.is_empty(),
            "{}: no receiving case",
            case.comment
        );
        for receiving in &case.receiving {
            assert!(receiving.expected.n_outputs.is_none());
            check_receiving(&case.comment, receiving);
            cases += 1;
        }
    }
    // 28 vectors; "use silent payments for sender change" has two receivers.
    assert_eq!(cases, 29);
}

/// The skip reasons reported for the two "not eligible" vectors.
#[test]
fn not_eligible_vectors_report_the_bip352_reason() {
    let all = vectors();
    let outcome = |comment: &str| {
        let case = all.iter().find(|c| c.comment == comment).unwrap();
        let given = &case.receiving[0].given;
        let (keys, _) = receiver(given);
        let (tx, prevouts) = build_tx(given);
        ReceiveScanner::with_exact_labels(&keys, &given.labels)
            .unwrap()
            .scan_tx(&tx, &prevouts)
            .unwrap()
    };
    assert_eq!(
        outcome("No valid inputs, sender generates no outputs"),
        TxScanOutcome::NotEligible(SkipReason::NoEligibleInputs)
    );
    assert_eq!(
        outcome("Input keys sum up to zero / point at infinity: sending fails, receiver skips tx"),
        TxScanOutcome::NotEligible(SkipReason::InputKeySumIsInfinity)
    );
}

/// BIP352 v1.1.0+: with 2324 matching outputs the receiver must find exactly
/// K_max = 2323 and stop.
#[test]
fn k_max_vector() {
    let all = vectors();
    let case = all.iter().find(|c| c.comment == K_MAX_VECTOR).unwrap();
    let receiving = &case.receiving[0];
    let expected_n = receiving.expected.n_outputs.unwrap();
    assert_eq!(expected_n, K_MAX as usize);
    assert_eq!(receiving.given.outputs.len(), expected_n + 1);

    let (keys, _) = receiver(&receiving.given);
    let (tx, prevouts) = build_tx(&receiving.given);
    let scanner = ReceiveScanner::with_exact_labels(&keys, &receiving.given.labels).unwrap();
    let outcome = scanner.scan_tx(&tx, &prevouts);
    eprintln!(
        "K_max vector outcome: {}",
        match &outcome {
            Ok(TxScanOutcome::Scanned { matches }) => format!("Scanned, {} matches", matches.len()),
            Ok(TxScanOutcome::NotEligible(r)) => format!("NotEligible({r:?})"),
            Err(e) => format!("Err({e})"),
        }
    );
    let matches = match outcome {
        Ok(TxScanOutcome::Scanned { matches }) => matches,
        other => panic!(
            "BIP352 expects {expected_n} detected outputs, got {:?}",
            other
                .map(|_| "non-scan outcome")
                .map_err(|e: ProtocolError| e.to_string())
        ),
    };
    assert_eq!(matches.len(), expected_n);

    // The capped result must refer to the *original* transaction.
    let txid = tx.compute_txid();
    let mut vouts = BTreeSet::new();
    for m in &matches {
        assert_eq!(m.outpoint.txid, txid);
        assert_eq!(
            m.script_pubkey,
            tx.output[m.outpoint.vout as usize].script_pubkey
        );
        assert!(vouts.insert(m.outpoint.vout), "output matched twice");
    }

    // Independent cross-check with a different upstream entry point: the
    // scripts for k = 0..K_max derived from the public tweak data are exactly
    // the matched outputs, and the single unmatched output is the one for
    // k = K_max, which BIP352 says must not be looked at.
    let (scan_sk, spend_pk) = {
        let secp = Secp256k1::new();
        let material = &receiving.given.key_material;
        (
            material.scan_priv_key,
            material.spend_priv_key.public_key(&secp),
        )
    };
    assert_eq!(receiving.given.labels, vec![0]);
    // Label lookup for the change label, built as in upstream's example.
    let label_lookup = {
        let secp = Secp256k1::new();
        let code = bdk_sp::encoding::SilentPaymentCode::new_v0(
            scan_sk.public_key(&secp),
            spend_pk,
            Network::Regtest,
        );
        let label = bdk_sp::encoding::SilentPaymentCode::get_label(scan_sk, 0);
        let label_point = code
            .add_label(label)
            .unwrap()
            .spend
            .combine(&code.spend.negate(&secp))
            .unwrap();
        std::collections::BTreeMap::from([(label_point, (label, 0u32))])
    };
    let upstream = bdk_sp::receive::scan::Scanner::new(scan_sk, spend_pk, label_lookup);
    let tweak = ReceiveScanner::public_tweak_data(&tx, &prevouts)
        .unwrap()
        .unwrap();
    let present: BTreeSet<&ScriptBuf> = tx.output.iter().map(|o| &o.script_pubkey).collect();
    // The one candidate script (unlabelled or change-labelled) for `k` that
    // is actually an output of the transaction.
    let script_for = |k: u32| {
        let candidates = upstream.get_spks_from_tweak(&tweak, k);
        assert_eq!(candidates.len(), 2, "change label + unlabelled");
        let hits: Vec<ScriptBuf> = candidates
            .into_iter()
            .filter(|c| present.contains(c))
            .collect();
        assert_eq!(hits.len(), 1, "k = {k}: exactly one output expected");
        hits[0].clone()
    };
    let within_limit: BTreeSet<ScriptBuf> = (0..K_MAX).map(script_for).collect();
    let matched: BTreeSet<ScriptBuf> = matches.iter().map(|m| m.script_pubkey.clone()).collect();
    assert_eq!(within_limit.len(), expected_n);
    assert!(
        matched == within_limit,
        "matched outputs differ from the scripts for k = 0..K_max ({} in common)",
        matched.intersection(&within_limit).count()
    );
    let unmatched: Vec<&TxOut> = tx
        .output
        .iter()
        .enumerate()
        .filter(|(i, _)| !vouts.contains(&(*i as u32)))
        .map(|(_, o)| o)
        .collect();
    assert_eq!(unmatched.len(), 1);
    assert!(unmatched[0].script_pubkey == script_for(K_MAX));
}

// ---------------------------------------------------------------------------
// Beyond the vectors: the K_max correction (docs/DECISIONS.md D-005) on a
// transaction the single official vector does not resemble.
// ---------------------------------------------------------------------------

/// Label lookup built as in upstream's `examples/send_and_scan.rs`.
fn upstream_label_lookup(
    scan_sk: SecretKey,
    spend_pk: PublicKey,
    labels: &[u32],
) -> std::collections::BTreeMap<PublicKey, (Scalar, u32)> {
    let secp = Secp256k1::new();
    let code = bdk_sp::encoding::SilentPaymentCode::new_v0(
        scan_sk.public_key(&secp),
        spend_pk,
        Network::Regtest,
    );
    labels
        .iter()
        .map(|&m| {
            let label = bdk_sp::encoding::SilentPaymentCode::get_label(scan_sk, m);
            let point = code
                .add_label(label)
                .unwrap()
                .spend
                .combine(&code.spend.negate(&secp))
                .unwrap();
            (point, (label, m))
        })
        .collect()
}

/// K_max + 3 outputs for one receiver, alternating between the unlabelled
/// address, the change label and label 7, placed in *reverse* k order with
/// unrelated outputs in between (including a taproot output that is not a
/// valid key). BIP352: exactly the outputs for k = 0..K_max are found, each
/// with its own vout, value and label; the three beyond the limit are not.
#[test]
fn k_max_cap_with_mixed_labels_reverse_order_and_several_outputs_beyond_the_limit() {
    use bdk_sp::receive::scan::Scanner;

    let all = vectors();
    let case = all.iter().find(|c| c.comment == K_MAX_VECTOR).unwrap();
    let given = &case.receiving[0].given;
    let (keys, _) = receiver(given);
    let secp = Secp256k1::new();
    let scan_sk = given.key_material.scan_priv_key;
    let spend_pk = given.key_material.spend_priv_key.public_key(&secp);
    // Inputs (and therefore the shared secret) are the vector's; outputs are ours.
    let (mut tx, prevouts) = build_tx(given);
    let tweak = ReceiveScanner::public_tweak_data(&tx, &prevouts)
        .unwrap()
        .unwrap();

    const LABEL: u32 = 7;
    let kinds = [None, Some(0), Some(LABEL)];
    let plain = Scanner::new(scan_sk, spend_pk, Default::default());
    let labelled = |m: u32| {
        Scanner::new(
            scan_sk,
            spend_pk,
            upstream_label_lookup(scan_sk, spend_pk, &[m]),
        )
    };
    let (change, seven) = (labelled(0), labelled(LABEL));
    // `get_spks_from_tweak` lists the labelled scripts first, the unlabelled last.
    let script_for = |k: u32| match kinds[k as usize % 3] {
        None => plain.get_spks_from_tweak(&tweak, k).remove(0),
        Some(0) => change.get_spks_from_tweak(&tweak, k).remove(0),
        Some(_) => seven.get_spks_from_tweak(&tweak, k).remove(0),
    };

    let total = K_MAX + 3;
    let mut output = Vec::new();
    let mut expected = BTreeSet::new(); // (vout, value, label)
    let mut beyond = BTreeSet::new(); // vouts for k >= K_max
    for k in (0..total).rev() {
        if k % 400 == 0 {
            // Junk: P2TR whose program is not a curve point, a P2WPKH output,
            // an empty script.
            let mut not_a_key = vec![0x51, 0x20];
            not_a_key.extend_from_slice(&[0xff; 32]);
            for script in [
                ScriptBuf::from_bytes(not_a_key),
                ScriptBuf::from_hex("001453d9c40342ee880e766522c3e2b854d37f2b3cbf").unwrap(),
                ScriptBuf::new(),
            ] {
                output.push(TxOut {
                    value: Amount::from_sat(5),
                    script_pubkey: script,
                });
            }
        }
        let vout = output.len() as u32;
        let value = 1_000 + u64::from(k);
        output.push(TxOut {
            value: Amount::from_sat(value),
            script_pubkey: script_for(k),
        });
        if k < K_MAX {
            expected.insert((vout, value, kinds[k as usize % 3]));
        } else {
            beyond.insert(vout);
        }
    }
    tx.output = output;
    assert_eq!((expected.len(), beyond.len()), (K_MAX as usize, 3));

    let scanner = ReceiveScanner::new(&keys, &[LABEL]).unwrap();
    let matches = match scanner.scan_tx(&tx, &prevouts) {
        Ok(TxScanOutcome::Scanned { matches }) => matches,
        other => panic!(
            "expected a capped scan, got {:?}",
            other.map(|_| "non-scan outcome")
        ),
    };
    let txid = tx.compute_txid();
    assert!(matches.iter().all(|m| m.outpoint.txid == txid));
    let found: BTreeSet<(u32, u64, Option<u32>)> = matches
        .iter()
        .map(|m| (m.outpoint.vout, m.amount.to_sat(), m.label))
        .collect();
    assert_eq!(matches.len(), K_MAX as usize);
    assert!(
        found == expected,
        "capped result differs from the outputs for k = 0..K_max"
    );
    assert!(found.iter().all(|(vout, _, _)| !beyond.contains(vout)));
}
