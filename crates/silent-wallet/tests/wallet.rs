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
    SendError, SendLimits, Settlement, Wallet, WalletKeys,
};

/// A seed made when the test runs: nothing that could be a key is kept in
/// the repository.
fn random_seed_hex() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .expect("a source of randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

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
    let seed = &random_seed_hex();
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
    let other = WalletKeys::from_seed_hex(&random_seed_hex(), Network::Regtest)
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
    // The change label is the wallet's own: it is not an address to hand out.
    assert!(wallet.address(Some(0)).is_err());

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
        confirmed_at: None,
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
    // What is reported as available leaves out outputs that are worth less
    // than the fee for spending them (the fixture has one of 1 sat).
    let worthwhile: Vec<&DetectedOutput> =
        candidates.iter().filter(|c| c.value_sats > 100).collect();
    assert!(worthwhile.len() < candidates.len());
    let usable: u64 = worthwhile.iter().map(|c| c.value_sats).sum();
    for amount in [available + 1, usable, usable - 50] {
        match build(&candidates, &pay(amount), 1, &limits) {
            Err(SendError::InsufficientFunds {
                available: reported,
                outputs,
                needed,
            }) => {
                assert_eq!((reported, outputs), (usable, worthwhile.len()), "{amount}");
                assert!(needed > usable, "{amount}: {needed}");
            }
            other => panic!("{amount}: {other:?}"),
        }
    }
    assert!(matches!(
        build(&[], &pay(10_000), 1, &limits),
        Err(SendError::InsufficientFunds { available: 0, outputs: 0, needed }) if needed > 10_000
    ));
    // Amounts that cannot exist are refused before anything is added up.
    let max_money = 21_000_000 * 100_000_000u64;
    for amount in [max_money + 1, u64::MAX] {
        assert_eq!(
            build(&candidates, &pay(amount), 1, &limits),
            Err(SendError::AmountTooLarge { max: max_money }),
            "{amount}"
        );
    }
    let two = vec![
        Payment {
            to: to(),
            amount_sats: max_money,
        },
        Payment {
            to: to(),
            amount_sats: 1_000,
        },
    ];
    assert_eq!(
        build(&candidates, &two, 1, &limits),
        Err(SendError::AmountTooLarge { max: max_money })
    );
    // The same output offered twice is not spent twice.
    let twice = vec![
        worthwhile[0].clone(),
        worthwhile[1].clone(),
        worthwhile[0].clone(),
    ];
    assert_eq!(
        build(&twice, &pay(330), 1, &limits),
        Err(SendError::DuplicateInput {
            outpoint: OutPoint::new(worthwhile[0].txid, worthwhile[0].vout)
        })
    );
    // A fee that is large next to the payment: 330 sat at a rate that costs
    // tens of thousands.
    match build(&candidates, &pay(330), 300, &limits) {
        Err(SendError::FeeTooHighForAmount {
            fee_sats,
            paid_sats: 330,
            max_percent: 50,
        }) => assert!(fee_sats > 10_000, "{fee_sats}"),
        other => panic!("{other:?}"),
    }
    // Allowed when the caller raises the limit on purpose.
    assert_eq!(
        build(
            &candidates,
            &pay(330),
            300,
            &SendLimits::default().with_max_fee_percent(100_000)
        ),
        Ok(())
    );
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
fn sent_transactions_stay_on_record_while_their_block_can_still_be_abandoned() {
    let dir = std::env::temp_dir().join(format!("silent-wallet-pending-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.sqlite3.pending.json");
    assert_eq!(
        PendingSpends::load(&path).unwrap(),
        PendingSpends::default()
    );

    let outpoint = |n: u8| OutPoint::new(Txid::from_byte_array([n; 32]), u32::from(n));
    let txid = |n: u8| Txid::from_byte_array([n; 32]);
    let sent = |n: u8, inputs: &[u8]| PendingTransaction {
        txid: txid(n),
        inputs: inputs.iter().map(|i| outpoint(*i)).collect(),
        sent_at_height: 8,
        confirmed_at: None,
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

    let spend = |input: u8, by: u8, height: u32| SpentOutput {
        txid: outpoint(input).txid,
        vout: outpoint(input).vout,
        spent_in: block_refs(height, height)[0],
        spending_txid: txid(by),
    };
    let outcome = |changes: Vec<(PendingTransaction, Settlement)>| {
        let mut list: Vec<(Txid, Settlement, Option<u32>)> = changes
            .into_iter()
            .map(|(t, change)| (t.txid, change, t.confirmed_at))
            .collect();
        list.sort_by_key(|(id, _, _)| *id);
        list
    };
    // Block 6: 100 confirmed (both inputs spent by it); 101's input was
    // spent by another transaction, so it can never confirm; 102 waits.
    let block_6 = [spend(1, 100, 6), spend(2, 100, 6), spend(3, 55, 6)];
    assert_eq!(
        outcome(pending.settle(&block_6, 6)),
        vec![
            (txid(100), Settlement::Confirmed, Some(6)),
            (txid(101), Settlement::Conflicted, None),
        ]
    );
    // The confirmed one stays on record — its block can still be abandoned —
    // but its inputs are the store's business now, not "pending".
    let mut on_record: Vec<(Txid, Option<u32>)> = pending
        .transactions()
        .iter()
        .map(|t| (t.txid, t.confirmed_at))
        .collect();
    on_record.sort();
    assert_eq!(on_record, vec![(txid(100), Some(6)), (txid(102), None)]);
    assert_eq!(
        pending.inputs().into_iter().collect::<Vec<_>>(),
        vec![outpoint(4)]
    );
    // Nothing new: nothing to report.
    assert!(pending.settle(&block_6, 7).is_empty());
    // Only one of two inputs recorded as spent is not a confirmation.
    let mut partial = PendingSpends::default();
    partial.record(sent(100, &[1, 2]));
    assert!(partial.settle(&[spend(1, 100, 6)], 6).is_empty());
    assert_eq!(partial.inputs().len(), 2);

    // The block is abandoned: the store no longer records the spends. The
    // transaction is waiting again, and its inputs are held back again.
    assert_eq!(
        outcome(pending.settle(&[], 5)),
        vec![(txid(100), Settlement::Unconfirmed, None)]
    );
    assert_eq!(pending.inputs().len(), 3);
    // It confirms again, in another block.
    let block_7 = [spend(1, 100, 7), spend(2, 100, 7)];
    assert_eq!(
        outcome(pending.settle(&block_7, 7)),
        vec![(txid(100), Settlement::Confirmed, Some(7))]
    );
    // It is kept until it is as deep as a reorganization is followed ...
    let keep = silent_wallet::pending::KEEP_CONFIRMED_FOR;
    assert!(pending.settle(&block_7, 7 + keep - 1).is_empty());
    assert_eq!(pending.transactions().len(), 2);
    // ... and then forgotten, silently.
    assert!(pending.settle(&block_7, 7 + keep).is_empty());
    assert_eq!(
        pending
            .transactions()
            .iter()
            .map(|t| t.txid)
            .collect::<Vec<_>>(),
        vec![txid(102)]
    );

    assert!(!pending.forget(txid(100)));
    assert!(pending.forget(txid(102)));
    assert!(pending.inputs().is_empty());

    // A record written before confirmations were tracked is still read.
    std::fs::write(
        &path,
        format!(
            r#"{{"transactions":[{{"txid":"{}","inputs":["{}"],"sent_at_height":3}}]}}"#,
            txid(7),
            outpoint(7)
        ),
    )
    .unwrap();
    let old = PendingSpends::load(&path).unwrap();
    assert_eq!(old.transactions()[0].confirmed_at, None);
    assert_eq!(old.inputs().len(), 1);
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
            matches!(CoreRpc::new(url, &cookie), Err(RpcError::Url)),
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

/// Change exists from exactly the smallest amount the network relays; one
/// satoshi less and it becomes fee. Outputs worth less than the fee for
/// spending them are left alone. The transaction does not ask to be
/// replaceable.
#[test]
fn change_starts_at_the_relay_minimum_and_worthless_inputs_are_skipped() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let to = || {
        Destination::parse(
            &fixture_gen::other_receiver_keys()
                .address(Network::Regtest)
                .to_string(),
            Network::Regtest,
        )
        .unwrap()
    };
    let big = candidates
        .iter()
        .find(|c| c.value_sats > 5_000)
        .unwrap()
        .clone();
    let build = |candidates: &[DetectedOutput], amount_sats: u64, rate: u64| {
        build_and_sign(
            &wallet,
            &source,
            candidates,
            &[Payment {
                to: to(),
                amount_sats,
            }],
            rate,
            &SendLimits::default(),
        )
    };
    let rate = 2;
    let with_change = build(std::slice::from_ref(&big), 1_000, rate).unwrap();
    let fee_with_change = with_change.vsize * rate;
    assert_eq!(with_change.fee_sats, fee_with_change);
    assert_eq!(with_change.fee_rate_asked, rate);

    // Exactly 330 sat left after the fee: a change output of 330 sat.
    let exact = build(
        std::slice::from_ref(&big),
        big.value_sats - fee_with_change - 330,
        rate,
    )
    .unwrap();
    assert_eq!(exact.change.as_ref().map(|c| c.amount_sats), Some(330));
    assert_eq!(exact.fee_sats, fee_with_change);
    // 329 sat left: no change, and those 329 sat are fee — no more than that.
    let below = build(
        std::slice::from_ref(&big),
        big.value_sats - fee_with_change - 329,
        rate,
    )
    .unwrap();
    assert_eq!(below.change, None);
    assert_eq!(below.fee_sats, fee_with_change + 329);
    assert!(below.fee_sats < below.vsize * rate + 330 + 43 * rate);

    // Not replaceable by signalling, and not time-locked.
    for input in &exact.transaction.input {
        assert_eq!(
            input.sequence,
            silent_sync::bitcoin::Sequence::ENABLE_LOCKTIME_NO_RBF
        );
        assert!(!input.sequence.is_rbf());
    }
    assert_eq!(
        exact.transaction.lock_time,
        silent_sync::bitcoin::absolute::LockTime::ZERO
    );

    // The fixture's 1 sat output costs more to spend than it holds: it is
    // never taken, wherever it stands among the candidates.
    let tiny = candidates
        .iter()
        .find(|c| c.value_sats == 1)
        .expect("the fixture has a 1 sat output")
        .clone();
    let built = build(&[tiny.clone(), big.clone()], 1_000, rate).unwrap();
    assert_eq!(built.inputs.len(), 1);
    assert_eq!(built.inputs[0].outpoint, OutPoint::new(big.txid, big.vout));
    assert!(matches!(
        build(std::slice::from_ref(&tiny), 330, 1),
        Err(SendError::InsufficientFunds {
            available: 0,
            outputs: 0,
            ..
        })
    ));
}

/// The check that guards the change is a live one: paying one's own address
/// under a label the wallet does not scan for hides the outputs after it in
/// the same transaction from the wallet — the change among them. Such a
/// transaction is not handed out.
#[test]
fn change_the_wallet_could_not_find_again_is_never_sent() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    // The wallet scans for LABEL_USED; 9 is a label of the same keys that it
    // does not scan for.
    let unscanned = wallet
        .receiver()
        .labelled_address(Network::Regtest, 9)
        .unwrap();
    let to_own_unknown_label =
        Destination::parse(&unscanned.to_string(), Network::Regtest).unwrap();
    let result = build_and_sign(
        &wallet,
        &source,
        &candidates,
        &[Payment {
            to: to_own_unknown_label,
            amount_sats: 2_000,
        }],
        1,
        &SendLimits::default(),
    );
    assert_eq!(
        result.map(|_| ()),
        Err(SendError::Check(
            "the wallet's own scanner does not find the change output"
        ))
    );
}

/// What the node answers is taken at its word only where it is one: a
/// refusal is a refusal, and everything else leaves the outcome unknown.
#[test]
fn answers_of_the_node_are_classified_by_what_they_prove() {
    use silent_wallet::{CoreRpc, RpcError};
    use std::io::{Read as _, Write as _};

    /// Serves one canned HTTP answer per request on a loopback port.
    fn serve(
        answers: Vec<Option<(u16, String)>>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for answer in answers {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..n]);
                    let text = String::from_utf8_lossy(&request);
                    let complete = text.split_once("\r\n\r\n").is_some_and(|(head, body)| {
                        head.lines()
                            .find_map(|l| l.strip_prefix("Content-Length: "))
                            .and_then(|l| l.trim().parse::<usize>().ok())
                            .is_some_and(|len| body.len() >= len)
                    });
                    if complete || n == 0 {
                        break;
                    }
                }
                seen.push(String::from_utf8_lossy(&request).to_string());
                // `None`: the request arrived, the answer never leaves.
                if let Some((status, body)) = answer {
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    );
                }
            }
            seen
        });
        (url, handle)
    }
    let dir = std::env::temp_dir().join(format!("silent-wallet-rpc2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cookie = dir.join(".cookie");
    std::fs::write(&cookie, "__cookie__:topsecretvalue\n").unwrap();
    let tx = silent_sync::bitcoin::Transaction {
        version: silent_sync::bitcoin::transaction::Version::TWO,
        lock_time: silent_sync::bitcoin::absolute::LockTime::ZERO,
        input: vec![],
        output: vec![],
    };
    let txid = tx.compute_txid();
    let ok = |result: String| {
        Some((
            200,
            format!(r#"{{"result":{result},"error":null,"id":"x"}}"#),
        ))
    };
    let node_error = |status: u16, message: &str| {
        Some((
            status,
            format!(r#"{{"result":null,"error":{{"code":-26,"message":"{message}"}},"id":"x"}}"#),
        ))
    };
    let send_with = |answer: Option<(u16, String)>| {
        let (url, server) = serve(vec![answer]);
        let result = CoreRpc::new(&url, &cookie).unwrap().send(&tx);
        let requests = server.join().unwrap();
        // The credentials go out as the Authorization header, the
        // transaction as the parameter, and neither shows in an error.
        assert!(requests[0].contains("Authorization: Basic X19jb29raWVfXzp0b3BzZWNyZXR2YWx1ZQ=="));
        assert!(requests[0].contains(r#""method":"sendrawtransaction""#));
        if let Err(e) = &result {
            assert!(!e.to_string().contains("topsecret") && !e.to_string().contains("X19jb29r"));
        }
        result
    };

    // The node took it and says so.
    assert_eq!(send_with(ok(format!("\"{txid}\""))), Ok(txid));
    // The node says no: certainly not sent.
    for (answer, expected) in [
        (
            node_error(500, "txn-mempool-conflict"),
            RpcError::Rejected("txn-mempool-conflict".to_string()),
        ),
        (
            node_error(404, "Method not found"),
            RpcError::Rejected("Method not found".to_string()),
        ),
        (Some((401, String::new())), RpcError::Unauthorized),
        (Some((403, String::new())), RpcError::Unauthorized),
    ] {
        let error = send_with(answer).unwrap_err();
        assert_eq!(error, expected);
        assert!(error.means_not_done(), "{error:?}");
    }
    // Everything else: the request went out, and nothing proves what the
    // node did with it.
    let other_txid = format!("\"{}\"", Txid::from_byte_array([9; 32]));
    for answer in [
        None,
        Some((200, "not json".to_string())),
        Some((502, "<html>bad gateway</html>".to_string())),
        Some((500, r#"{"result":null,"error":null}"#.to_string())),
        ok("null".to_string()),
        ok("\"zz\"".to_string()),
        ok(other_txid),
        Some((200, "x".repeat(2 * 1024 * 1024))),
    ] {
        let shown = format!("{:?}", answer.as_ref().map(|(s, b)| (s, b.len())));
        let error = send_with(answer).unwrap_err();
        assert!(
            matches!(error, RpcError::NoAnswer(_) | RpcError::Malformed(_)),
            "{shown}: {error:?}"
        );
        assert!(!error.means_not_done(), "{shown}: {error:?}");
    }
    // Nobody listening: never sent.
    let unreachable = CoreRpc::new("http://127.0.0.1:9", &cookie)
        .unwrap()
        .send(&tx)
        .unwrap_err();
    assert!(matches!(unreachable, RpcError::Unavailable(_)) && unreachable.means_not_done());

    // Would the node accept it? Only an explicit yes counts.
    let accept_with = |answer: Option<(u16, String)>| {
        let (url, server) = serve(vec![answer]);
        let result = CoreRpc::new(&url, &cookie).unwrap().would_accept(&tx);
        server.join().unwrap();
        result
    };
    assert_eq!(accept_with(ok(r#"[{"allowed":true}]"#.to_string())), Ok(()));
    assert_eq!(
        accept_with(ok(
            r#"[{"allowed":false,"reject-reason":"missing-inputs"}]"#.to_string()
        )),
        Err(RpcError::Rejected("missing-inputs".to_string()))
    );
    for vague in [r#"[]"#, r#"[{}]"#, r#"{"allowed":true}"#, "null"] {
        assert!(accept_with(ok(vague.to_string())).is_err(), "{vague}");
    }
    // A "yes" under an HTTP status that is not success is not a yes.
    let yes_body = ok(r#"[{"allowed":true}]"#.to_string()).map(|(_, body)| body);
    for status in [500, 404, 302] {
        assert!(
            accept_with(yes_body.clone().map(|body| (status, body))).is_err(),
            "{status}"
        );
    }

    // Which chain is the node on? Its genesis block must be the store's.
    let regtest = silent_sync::ChainIdentity::for_network(Network::Regtest).genesis_hash();
    let signet = silent_sync::ChainIdentity::for_network(Network::Signet).genesis_hash();
    let chain_with = |answer: Option<(u16, String)>, expected| {
        let (url, server) = serve(vec![answer]);
        let result = CoreRpc::new(&url, &cookie).unwrap().check_chain(expected);
        let requests = server.join().unwrap();
        assert!(requests[0].contains(r#""method":"getblockhash""#));
        result
    };
    assert_eq!(chain_with(ok(format!("\"{regtest}\"")), regtest), Ok(()));
    assert_eq!(
        chain_with(ok(format!("\"{signet}\"")), regtest),
        Err(RpcError::WrongChain)
    );
    let mainnet_genesis = "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f";
    assert_eq!(
        chain_with(ok(format!("\"{mainnet_genesis}\"")), regtest),
        Err(RpcError::WrongChain)
    );
    assert!(chain_with(ok("null".to_string()), regtest).is_err());

    // A URL with credentials in it is refused without repeating them.
    let refused =
        CoreRpc::new("http://rpcuser:hunter2secret@127.0.0.1:18443", &cookie).unwrap_err();
    assert_eq!(refused, RpcError::Url);
    assert!(!refused.to_string().contains("hunter2"));
    // A cookie file that is not a cookie.
    for contents in ["", "no-colon", "a:b\nc:d"] {
        std::fs::write(&cookie, contents).unwrap();
        assert!(
            matches!(
                CoreRpc::new("http://127.0.0.1:9", &cookie),
                Err(RpcError::Cookie { .. })
            ),
            "{contents:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
