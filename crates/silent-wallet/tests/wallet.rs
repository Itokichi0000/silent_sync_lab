//! The wallet on the offline fixture chain: keys, what counts as unspent,
//! and building and signing a payment that the recipient's scanner — and,
//! for the change, the wallet's own — then finds.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LABEL_USED, LAB_RECEIVER},
    sync_suite::block_refs,
    test_source::TestSource,
};
use silent_sync::{
    bitcoin::{
        hashes::Hash as _,
        secp256k1::{Message, Secp256k1, XOnlyPublicKey},
        sighash::{Prevouts, SighashCache},
        OutPoint, TapSighashType, TxOut, Txid,
    },
    new_store_meta,
    protocol::TxScanOutcome,
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
    },
    spend::{recover_spendable, SpendError},
    sync, DetectedOutput, EvidenceKind, MemoryStore, Network, ReceiveScanner, ScanSource,
    SpentOutput, SyncOptions, SyncStore,
};
use silent_wallet::{
    build_and_sign, holdings, Destination, KeyError, Payment, PendingSpends, PendingTransaction,
    SendError, SendLimits, Wallet, WalletKeys,
};

/// The wallet of the fixture's receiver: its private keys are published
/// BIP352 test values.
fn wallet() -> Wallet {
    let (scan, spend) = fixture_gen::vector_secret_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    Wallet::new(
        WalletKeys::from_secret_keys(scan, spend),
        Network::Regtest,
        &[LABEL_USED],
    )
    .unwrap()
}

fn synced(source: &dyn ScanSource, wallet: &Wallet) -> MemoryStore {
    let mut store = MemoryStore::new();
    store
        .initialize(&new_store_meta(source, wallet.scanner(), 1, "test"))
        .unwrap();
    sync(
        source,
        wallet.scanner(),
        &mut store,
        &SyncOptions::default(),
    )
    .unwrap();
    store
}

fn unspent(store: &MemoryStore) -> Vec<DetectedOutput> {
    holdings(store, 1, &PendingSpends::default())
        .unwrap()
        .unspent
        .into_iter()
        .map(|held| held.output)
        .collect()
}

#[test]
fn keys_come_from_the_seed_and_are_never_shown() {
    let seed = "000102030405060708090a0b0c0d0e0f";
    let keys = WalletKeys::from_seed_hex(seed, Network::Regtest).unwrap();
    let again = WalletKeys::from_seed_hex(&format!("  {seed}\n"), Network::Regtest).unwrap();
    let wallet = Wallet::new(keys.clone(), Network::Regtest, &[]).unwrap();
    assert_eq!(
        wallet.address(None).unwrap(),
        Wallet::new(again, Network::Regtest, &[])
            .unwrap()
            .address(None)
            .unwrap()
    );
    // Scan and spend keys are different keys, and another seed gives others.
    let receiver = keys.receiver();
    assert_ne!(receiver.scan_public_key(), receiver.spend_public_key());
    let other = WalletKeys::from_seed_hex("ff0102030405060708090a0b0c0d0e0f", Network::Regtest)
        .unwrap()
        .receiver();
    assert_ne!(other.scan_public_key(), receiver.scan_public_key());
    assert_ne!(other.spend_public_key(), receiver.spend_public_key());
    // The same keys on signet; only the address prefix differs.
    let signet = Wallet::new(
        WalletKeys::from_seed_hex(seed, Network::Signet).unwrap(),
        Network::Signet,
        &[],
    )
    .unwrap();
    assert_eq!(signet.receiver().wallet_id(), receiver.wallet_id());
    assert!(signet
        .address(None)
        .unwrap()
        .to_string()
        .starts_with("tsp1"));
    assert!(wallet
        .address(None)
        .unwrap()
        .to_string()
        .starts_with("sprt1"));
    assert_ne!(
        wallet.address(None).unwrap().to_string(),
        wallet.address(Some(3)).unwrap().to_string()
    );

    for shown in [format!("{keys:?}"), format!("{wallet:?}")] {
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(!shown.contains(seed));
    }
    for bad in ["", "zz", "0001", &"00".repeat(15), &"00".repeat(65), "0x00"] {
        assert_eq!(
            WalletKeys::from_seed_hex(bad, Network::Regtest).err(),
            Some(KeyError::InvalidSeed),
            "`{bad}`"
        );
    }
    // The error does not repeat what was given.
    assert!(!KeyError::InvalidSeed.to_string().contains("zz"));
}

/// "Unspent" is: detected through full blocks, no spend recorded, enough
/// confirmations, not an input of a transaction that was sent. An index's
/// claims are listed apart and are part of no sum.
#[test]
fn only_what_full_blocks_show_unspent_is_counted() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let all = store.detections().unwrap();
    assert_eq!(all.len(), 7);
    let total: u64 = all.iter().map(|d| d.value_sats).sum();

    let held = holdings(&store, 1, &PendingSpends::default()).unwrap();
    assert_eq!(held.as_of, store.checkpoint().unwrap());
    assert_eq!(held.unspent.len(), 7);
    assert_eq!(held.unspent_sats(), total);
    assert_eq!((held.spent, held.index_claims.len()), (0, 0));
    assert!(held.unconfirmed_enough.is_empty() && held.spent_by_pending.is_empty());
    // Confirmations count the block itself.
    for h in &held.unspent {
        assert_eq!(h.confirmations, 8 - h.output.block.height + 1);
    }

    // More confirmations asked for: the newest outputs wait.
    let strict = holdings(&store, 3, &PendingSpends::default()).unwrap();
    assert!(strict.unspent.iter().all(|h| h.confirmations >= 3));
    assert!(strict
        .unconfirmed_enough
        .iter()
        .all(|h| h.confirmations < 3));
    assert!(!strict.unconfirmed_enough.is_empty());
    assert_eq!(strict.unspent.len() + strict.unconfirmed_enough.len(), 7);
    assert!(strict.unspent_sats() < total);

    // A recorded spend, and an input of a transaction that was sent.
    let mut with_spend = MemoryStore::new();
    with_spend
        .initialize(&new_store_meta(&source, wallet.scanner(), 1, "test"))
        .unwrap();
    let spend = SpentOutput {
        txid: all[0].txid,
        vout: all[0].vout,
        spent_in: block_refs(6, 6)[0],
        spending_txid: Txid::from_byte_array([1; 32]),
    };
    with_spend
        .append(&block_refs(1, 8), &all, &[spend])
        .unwrap();
    let mut pending = PendingSpends::default();
    pending.record(PendingTransaction {
        txid: Txid::from_byte_array([2; 32]),
        inputs: vec![OutPoint::new(all[1].txid, all[1].vout)],
        sent_at_height: 8,
    });
    let held = holdings(&with_spend, 1, &pending).unwrap();
    assert_eq!(held.spent, 1);
    assert_eq!(held.spent_by_pending.len(), 1);
    assert_eq!(held.spent_by_pending[0].output, all[1]);
    assert_eq!(held.unspent.len(), 5);
    assert_eq!(
        held.unspent_sats(),
        total - all[0].value_sats - all[1].value_sats
    );

    // A store filled from an index: nothing in it counts.
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture_gen::generate()).unwrap(),
        IndexPolicy::Bip352,
    );
    let from_index = synced(&index, &wallet);
    let held = holdings(&from_index, 1, &PendingSpends::default()).unwrap();
    assert_eq!(held.index_claims.len(), 7);
    assert!(held
        .index_claims
        .iter()
        .all(|d| d.provenance.evidence_kind == EvidenceKind::TweakIndex));
    assert_eq!(
        (held.unspent.len(), held.unspent_sats(), held.spent),
        (0, 0, 0)
    );

    // Nothing scanned yet.
    let mut empty = MemoryStore::new();
    empty
        .initialize(&new_store_meta(&source, wallet.scanner(), 1, "test"))
        .unwrap();
    let held = holdings(&empty, 1, &PendingSpends::default()).unwrap();
    assert_eq!((held.as_of, held.unspent.len()), (None, 0));
}

/// The previous outputs of a built transaction, fetched again from the chain.
fn prevouts_of(
    source: &TestSource,
    wallet: &Wallet,
    inputs: &[OutPoint],
    from: &[DetectedOutput],
) -> Vec<TxOut> {
    inputs
        .iter()
        .map(|outpoint| {
            let d = from
                .iter()
                .find(|d| OutPoint::new(d.txid, d.vout) == *outpoint)
                .unwrap();
            recover_spendable(source, wallet.scanner(), d)
                .unwrap()
                .txout
        })
        .collect()
}

#[test]
fn a_payment_is_signed_validly_and_found_by_both_sides() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let available: u64 = candidates.iter().map(|d| d.value_sats).sum();
    let largest = candidates.iter().map(|d| d.value_sats).max().unwrap();

    let recipient_keys = fixture_gen::other_receiver_keys();
    let recipient = ReceiveScanner::new(&recipient_keys, &[]).unwrap();
    let to_recipient = Destination::parse(
        &recipient_keys.address(Network::Regtest).to_string(),
        Network::Regtest,
    )
    .unwrap();
    let ordinary = Destination::parse(
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        Network::Regtest,
    )
    .unwrap();
    // More than any single output holds, so that several inputs are needed.
    let amount = largest + 1_000;
    assert!(amount + 20_000 < available);
    let payments = vec![
        Payment {
            to: to_recipient.clone(),
            amount_sats: amount,
        },
        Payment {
            to: ordinary,
            amount_sats: 5_000,
        },
    ];
    let fee_rate = 3;
    let built = build_and_sign(
        &wallet,
        &source,
        &candidates,
        &payments,
        fee_rate,
        &SendLimits::default(),
    )
    .unwrap();
    let tx = &built.transaction;

    // Inputs: the candidates, in the order given, as many as needed.
    assert!(built.inputs.len() >= 2);
    for (input, candidate) in built.inputs.iter().zip(&candidates) {
        assert_eq!(
            input.outpoint,
            OutPoint::new(candidate.txid, candidate.vout)
        );
        assert_eq!(input.value_sats, candidate.value_sats);
    }
    assert_eq!(tx.input.len(), built.inputs.len());
    // Outputs: the payments in order, then the change.
    assert_eq!(built.payments.len(), 2);
    assert_eq!(tx.output[0].value.to_sat(), amount);
    assert_eq!(tx.output[1].value.to_sat(), 5_000);
    assert_eq!(
        tx.output[1].script_pubkey,
        silent_sync::bitcoin::Address::from_script(
            &tx.output[1].script_pubkey,
            silent_sync::bitcoin::Network::Regtest
        )
        .unwrap()
        .script_pubkey()
    );
    let change = built.change.clone().expect("change");
    assert_eq!(change.vout, 2);
    assert_eq!(tx.output.len(), 3);
    assert_eq!(tx.output[2].value.to_sat(), change.amount_sats);
    // The fee is what the inputs leave, and at least the rate asked for.
    let spent: u64 = built.inputs.iter().map(|i| i.value_sats).sum();
    let sent: u64 = tx.output.iter().map(|o| o.value.to_sat()).sum();
    assert_eq!(built.fee_sats, spent - sent);
    assert_eq!(built.vsize, tx.vsize() as u64);
    assert!(built.fee_sats >= built.vsize * fee_rate);
    assert!(
        built.fee_sats < built.vsize * (fee_rate + 1),
        "no hidden surplus"
    );
    assert_eq!(built.txid, tx.compute_txid());

    // Every input carries one 64-byte signature that verifies against the
    // key of the output it spends (checked here independently).
    let outpoints: Vec<OutPoint> = built.inputs.iter().map(|i| i.outpoint).collect();
    let prevouts = prevouts_of(&source, &wallet, &outpoints, &candidates);
    let secp = Secp256k1::verification_only();
    let mut cache = SighashCache::new(tx);
    for (index, prevout) in prevouts.iter().enumerate() {
        let witness = &tx.input[index].witness;
        assert_eq!(witness.len(), 1);
        let signature =
            silent_sync::bitcoin::secp256k1::schnorr::Signature::from_slice(&witness[0]).unwrap();
        let sighash = cache
            .taproot_key_spend_signature_hash(
                index,
                &Prevouts::All(&prevouts),
                TapSighashType::Default,
            )
            .unwrap();
        let key = XOnlyPublicKey::from_slice(&prevout.script_pubkey.as_bytes()[2..]).unwrap();
        secp.verify_schnorr(&signature, &Message::from(sighash), &key)
            .unwrap();
    }

    // The recipient's scanner finds exactly its payment ...
    let found = |scanner: &ReceiveScanner| match scanner.scan_tx(tx, &prevouts).unwrap() {
        TxScanOutcome::Scanned { matches } => matches
            .into_iter()
            .map(|m| (m.outpoint.vout, m.amount.to_sat(), m.label))
            .collect::<Vec<_>>(),
        TxScanOutcome::NotEligible(reason) => panic!("not eligible: {reason:?}"),
    };
    assert_eq!(found(&recipient), vec![(0, amount, None)]);
    // ... and the wallet's own finds its change, under the change label.
    assert_eq!(
        found(wallet.scanner()),
        vec![(2, change.amount_sats, Some(0))]
    );
    // Nothing secret is in what is returned.
    let (scan, spend) = fixture_gen::vector_secret_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    let shown = format!("{built:?}") + &serde_json::to_string(&built).unwrap();
    for secret in [scan, spend] {
        assert!(!shown.contains(&secret.display_secret().to_string()));
    }

    // A payment to the wallet itself, twice to the same labelled address:
    // three outputs for one receiver, each found, none sent to a key the
    // wallet cannot find.
    let own = Destination::parse(
        &wallet.address(Some(LABEL_USED)).unwrap().to_string(),
        Network::Regtest,
    )
    .unwrap();
    let to_self = vec![
        Payment {
            to: own.clone(),
            amount_sats: 2_000,
        },
        Payment {
            to: own,
            amount_sats: 3_000,
        },
    ];
    let built = build_and_sign(
        &wallet,
        &source,
        &candidates,
        &to_self,
        1,
        &SendLimits::default(),
    )
    .unwrap();
    let outpoints: Vec<OutPoint> = built.inputs.iter().map(|i| i.outpoint).collect();
    let prevouts = prevouts_of(&source, &wallet, &outpoints, &candidates);
    let tx = &built.transaction;
    let mut mine = match wallet.scanner().scan_tx(tx, &prevouts).unwrap() {
        TxScanOutcome::Scanned { matches } => matches
            .into_iter()
            .map(|m| (m.outpoint.vout, m.amount.to_sat(), m.label))
            .collect::<Vec<_>>(),
        TxScanOutcome::NotEligible(reason) => panic!("not eligible: {reason:?}"),
    };
    mine.sort();
    let change = built.change.unwrap();
    assert_eq!(
        mine,
        vec![
            (0, 2_000, Some(LABEL_USED)),
            (1, 3_000, Some(LABEL_USED)),
            (2, change.amount_sats, Some(0)),
        ]
    );
}

#[test]
fn what_is_left_below_the_relay_minimum_becomes_fee_not_change() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let first = candidates[0].value_sats;
    let to = Destination::parse(
        &fixture_gen::other_receiver_keys()
            .address(Network::Regtest)
            .to_string(),
        Network::Regtest,
    )
    .unwrap();
    // Leave 200 sat more than the fee of a one-input, one-output transaction.
    let fee_rate = 1;
    let probe = build_and_sign(
        &wallet,
        &source,
        &candidates[..1],
        &[Payment {
            to: to.clone(),
            amount_sats: 1_000,
        }],
        fee_rate,
        &SendLimits::default(),
    );
    // (the first fixture output may be too small for this probe; pick one
    // that is large enough)
    let (candidate, base_fee) = match probe {
        Ok(built) if built.change.is_some() => {
            let no_change_vsize = built.vsize - 43;
            (candidates[0].clone(), no_change_vsize)
        }
        _ => {
            let big = candidates
                .iter()
                .find(|c| c.value_sats > 5_000)
                .unwrap()
                .clone();
            let built = build_and_sign(
                &wallet,
                &source,
                std::slice::from_ref(&big),
                &[Payment {
                    to: to.clone(),
                    amount_sats: 1_000,
                }],
                fee_rate,
                &SendLimits::default(),
            )
            .unwrap();
            assert!(built.change.is_some());
            (big, built.vsize - 43)
        }
    };
    let _ = first;
    let amount = candidate.value_sats - base_fee - 200;
    let built = build_and_sign(
        &wallet,
        &source,
        std::slice::from_ref(&candidate),
        &[Payment {
            to,
            amount_sats: amount,
        }],
        fee_rate,
        &SendLimits::default(),
    )
    .unwrap();
    assert_eq!(built.change, None);
    assert_eq!(built.transaction.output.len(), 1);
    assert_eq!(built.fee_sats, candidate.value_sats - amount);
    assert_eq!(built.fee_sats, base_fee + 200);
}

#[test]
fn nothing_is_built_that_should_not_be_sent() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let available: u64 = candidates.iter().map(|d| d.value_sats).sum();
    let to = || {
        Destination::parse(
            &fixture_gen::other_receiver_keys()
                .address(Network::Regtest)
                .to_string(),
            Network::Regtest,
        )
        .unwrap()
    };
    let pay = |amount_sats: u64| {
        vec![Payment {
            to: to(),
            amount_sats,
        }]
    };
    let limits = SendLimits::default();
    let build =
        |candidates: &[DetectedOutput], payments: &[Payment], rate: u64, limits: &SendLimits| {
            build_and_sign(&wallet, &source, candidates, payments, rate, limits).map(|_| ())
        };

    assert_eq!(
        build(&candidates, &[], 1, &limits),
        Err(SendError::NoPayments)
    );
    // Fee rate: not zero, not above the limit.
    for rate in [0, limits.max_fee_rate + 1] {
        assert_eq!(
            build(&candidates, &pay(10_000), rate, &limits),
            Err(SendError::FeeRate {
                given: rate,
                max: limits.max_fee_rate
            })
        );
    }
    // An amount the network would not relay.
    assert!(matches!(
        build(&candidates, &pay(329), 1, &limits),
        Err(SendError::BelowDust {
            dust: 330,
            amount_sats: 329,
            ..
        })
    ));
    assert_eq!(build(&candidates, &pay(330), 1, &limits), Ok(()));
    // More than there is: everything, and everything minus less than a fee.
    for amount in [available + 1, available, available - 50] {
        match build(&candidates, &pay(amount), 1, &limits) {
            Err(SendError::InsufficientFunds {
                available: reported,
                outputs,
                needed,
            }) => {
                assert_eq!((reported, outputs), (available, candidates.len()));
                assert!(needed > available, "{amount}: {needed}");
            }
            other => panic!("{amount}: {other:?}"),
        }
    }
    assert!(matches!(
        build(&[], &pay(10_000), 1, &limits),
        Err(SendError::InsufficientFunds {
            available: 0,
            outputs: 0,
            ..
        })
    ));
    // The fee limit.
    let tight = SendLimits::default().with_max_fee_sats(100);
    assert!(matches!(
        build(&candidates, &pay(10_000), 1, &tight),
        Err(SendError::FeeTooHigh { max: 100, .. })
    ));
    // More inputs than allowed.
    let mut one_input = SendLimits::default();
    one_input.max_inputs = 1;
    let largest = candidates.iter().map(|d| d.value_sats).max().unwrap();
    assert_eq!(
        build(&candidates, &pay(largest + 1_000), 1, &one_input),
        Err(SendError::TooManyInputs { max: 1 })
    );

    // A candidate that is not what the chain has is not signed for.
    let mut altered = candidates.clone();
    altered[0].value_sats += 1;
    assert!(matches!(
        build(&altered, &pay(1_000), 1, &limits),
        Err(SendError::Input(SpendError::NotReproduced { .. }))
    ));
    let mut claimed = candidates.clone();
    claimed[0].provenance.evidence_kind = EvidenceKind::TweakIndex;
    assert!(matches!(
        build(&claimed, &pay(1_000), 1, &limits),
        Err(SendError::Input(
            SpendError::DetectionNotFromFullBlocks { .. }
        ))
    ));
    // The chain moved under the candidates.
    source.on_fork.set(true);
    let newest = candidates
        .iter()
        .find(|c| c.block.height >= 7)
        .unwrap()
        .clone();
    assert!(matches!(
        build(std::slice::from_ref(&newest), &pay(330), 1, &limits),
        Err(SendError::Input(SpendError::BlockChanged { .. }))
    ));
    source.on_fork.set(false);

    // Destinations for another network, mainnet included, are not read.
    for text in [
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
        "not an address",
        "",
    ] {
        assert!(
            matches!(
                Destination::parse(text, Network::Regtest),
                Err(SendError::Destination(_))
            ),
            "`{text}`"
        );
    }
    let mainnet = bdk_sp::encoding::SilentPaymentCode::new_v0(
        wallet.receiver().scan_public_key(),
        wallet.receiver().spend_public_key(),
        bdk_sp::bitcoin::Network::Bitcoin,
    )
    .to_string();
    let signet = wallet.receiver().address(Network::Signet).to_string();
    for text in [mainnet, signet] {
        assert!(matches!(
            Destination::parse(&text, Network::Regtest),
            Err(SendError::Destination(_))
        ));
    }
}

#[test]
fn sent_transactions_are_remembered_until_a_block_settles_them() {
    let dir = std::env::temp_dir().join(format!("silent-wallet-pending-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.sqlite3.pending.json");
    assert_eq!(
        PendingSpends::load(&path).unwrap(),
        PendingSpends::default()
    );

    let outpoint = |n: u8| OutPoint::new(Txid::from_byte_array([n; 32]), u32::from(n));
    let sent = |n: u8, inputs: &[u8]| PendingTransaction {
        txid: Txid::from_byte_array([n; 32]),
        inputs: inputs.iter().map(|i| outpoint(*i)).collect(),
        sent_at_height: 8,
    };
    let mut pending = PendingSpends::default();
    pending.record(sent(100, &[1, 2]));
    pending.record(sent(101, &[3]));
    pending.record(sent(102, &[4]));
    pending.record(sent(100, &[1, 2]));
    assert_eq!(pending.transactions().len(), 3);
    assert_eq!(pending.inputs().len(), 4);
    pending.save(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(PendingSpends::load(&path).unwrap(), pending);

    let spend = |input: u8, by: u8| SpentOutput {
        txid: outpoint(input).txid,
        vout: outpoint(input).vout,
        spent_in: block_refs(8, 8)[0],
        spending_txid: Txid::from_byte_array([by; 32]),
    };
    // 100 confirmed (both inputs spent by it); 101's input was spent by
    // another transaction: it can no longer confirm; 102 is still waiting.
    let settled = pending.settle(&[spend(1, 100), spend(2, 100), spend(3, 55)]);
    let mut outcome: Vec<(Txid, bool)> = settled
        .iter()
        .map(|(t, confirmed)| (t.txid, *confirmed))
        .collect();
    outcome.sort();
    assert_eq!(
        outcome,
        vec![
            (Txid::from_byte_array([100; 32]), true),
            (Txid::from_byte_array([101; 32]), false),
        ]
    );
    assert_eq!(pending.transactions(), &[sent(102, &[4])]);
    assert!(pending.settle(&[spend(9, 9)]).is_empty());
    assert!(!pending.forget(Txid::from_byte_array([100; 32])));
    assert!(pending.forget(Txid::from_byte_array([102; 32])));
    assert!(pending.inputs().is_empty());

    std::fs::write(&path, "not json").unwrap();
    assert!(PendingSpends::load(&path).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_node_connection_is_loopback_only_and_hides_the_cookie() {
    use silent_wallet::{CoreRpc, RpcError};
    let dir = std::env::temp_dir().join(format!("silent-wallet-rpc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cookie = dir.join(".cookie");
    std::fs::write(&cookie, "__cookie__:topsecretvalue\n").unwrap();

    for url in [
        "http://example.com:8332",
        "https://127.0.0.1:18443",
        "http://127.0.0.1",
        "http://0.0.0.0:18443",
        "http://192.168.1.5:18443",
        "http://[::1]:18443",
        "127.0.0.1:18443",
    ] {
        assert!(
            matches!(CoreRpc::new(url, &cookie), Err(RpcError::Url(_))),
            "{url}"
        );
    }
    let rpc = CoreRpc::new("http://127.0.0.1:9/", &cookie).unwrap();
    let shown = format!("{rpc:?}");
    assert!(shown.contains("<redacted>") && !shown.contains("topsecret"));
    assert!(!shown.contains("dG9wc2VjcmV0"), "not even encoded");

    // Errors about the cookie name the file, never its contents.
    let missing = CoreRpc::new("http://127.0.0.1:9", &dir.join("nope")).unwrap_err();
    assert!(matches!(missing, RpcError::Cookie { .. }));
    std::fs::write(&cookie, "topsecretvalue-without-a-colon").unwrap();
    let malformed = CoreRpc::new("http://127.0.0.1:9", &cookie).unwrap_err();
    assert!(!malformed.to_string().contains("topsecret"));

    // Nobody listens there: an error, not a hang and not a success.
    std::fs::write(&cookie, "__cookie__:topsecretvalue").unwrap();
    let rpc = CoreRpc::new("http://127.0.0.1:9", &cookie).unwrap();
    let tx = silent_sync::bitcoin::Transaction {
        version: silent_sync::bitcoin::transaction::Version::TWO,
        lock_time: silent_sync::bitcoin::absolute::LockTime::ZERO,
        input: vec![],
        output: vec![],
    };
    let error = rpc.send(&tx).unwrap_err();
    assert!(matches!(error, RpcError::Unavailable(_)), "{error:?}");
    assert!(!error.to_string().contains("topsecret"));
    let _ = std::fs::remove_dir_all(&dir);
}
