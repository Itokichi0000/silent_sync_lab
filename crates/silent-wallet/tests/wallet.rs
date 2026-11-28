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

    // Inputs: candidates, each once, as many as needed (in any order).
    assert!(built.inputs.len() >= 2);
    let mut seen = std::collections::BTreeSet::new();
    for (input, txin) in built.inputs.iter().zip(&tx.input) {
        let candidate = candidates
            .iter()
            .find(|c| OutPoint::new(c.txid, c.vout) == input.outpoint)
            .expect("an input that was not offered");
        assert_eq!(input.value_sats, candidate.value_sats);
        assert_eq!(txin.previous_output, input.outpoint);
        assert!(seen.insert(input.outpoint));
    }
    assert_eq!(tx.input.len(), built.inputs.len());
    // Outputs: the payments and the change, each where the result says.
    assert_eq!(built.payments.len(), 2);
    let (first, second) = (
        built.payments[0].vout as usize,
        built.payments[1].vout as usize,
    );
    assert_eq!(tx.output[first].value.to_sat(), amount);
    assert_eq!(tx.output[second].value.to_sat(), 5_000);
    assert_eq!(
        tx.output[second].script_pubkey,
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080"
            .parse::<silent_sync::bitcoin::Address<_>>()
            .unwrap()
            .assume_checked()
            .script_pubkey()
    );
    let change = built.change.clone().expect("change");
    assert_eq!(tx.output.len(), 3);
    let places: std::collections::BTreeSet<usize> = [first, second, change.vout as usize].into();
    assert_eq!(places.len(), 3);
    assert_eq!(
        tx.output[change.vout as usize].value.to_sat(),
        change.amount_sats
    );
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
    assert_eq!(found(&recipient), vec![(first as u32, amount, None)]);
    // ... and the wallet's own finds its change, under the change label.
    assert_eq!(
        found(wallet.scanner()),
        vec![(change.vout, change.amount_sats, Some(0))]
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
    let mut expected = vec![
        (built.payments[0].vout, 2_000, Some(LABEL_USED)),
        (built.payments[1].vout, 3_000, Some(LABEL_USED)),
        (change.vout, change.amount_sats, Some(0)),
    ];
    expected.sort();
    assert_eq!(mine, expected);
    assert_eq!(
        expected.iter().map(|e| e.0).collect::<Vec<_>>(),
        vec![0, 1, 2]
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

    // Not replaceable by signalling; the sequence lets the lock time apply.
    for input in &exact.transaction.input {
        assert_eq!(
            input.sequence,
            silent_sync::bitcoin::Sequence::ENABLE_LOCKTIME_NO_RBF
        );
        assert!(!input.sequence.is_rbf());
        assert!(input.sequence.enables_absolute_lock_time());
    }

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

/// Mainnet is this crate's own decision. Built without its `mainnet`
/// feature it makes no wallet for mainnet — also when `silent-sync` can name
/// mainnet because another crate in the build asked for it. (When it cannot,
/// there is no value to refuse.)
#[cfg(not(feature = "mainnet"))]
#[test]
fn no_wallet_is_made_for_mainnet_without_the_feature() {
    use silent_wallet::WalletError;
    let Ok(mainnet) = "mainnet".parse::<Network>() else {
        return;
    };
    assert!(mainnet.is_mainnet());
    let keys = WalletKeys::from_seed_hex(&random_seed_hex(), mainnet).unwrap();
    assert_eq!(
        Wallet::new(keys, mainnet, &[]).err(),
        Some(WalletError::MainnetNotEnabled)
    );
}

/// With the feature: a mainnet wallet has keys of its own (coin type 0), so
/// a seed used on a test network does not give the same wallet on mainnet,
/// and its address carries the mainnet prefix.
#[cfg(feature = "mainnet")]
#[test]
fn a_mainnet_wallet_has_keys_of_its_own() {
    let seed = random_seed_hex();
    let wallet_on = |network: Network| {
        Wallet::new(
            WalletKeys::from_seed_hex(&seed, network).unwrap(),
            network,
            &[],
        )
        .unwrap()
    };
    let (mainnet, signet, regtest) = (
        wallet_on(Network::Mainnet),
        wallet_on(Network::Signet),
        wallet_on(Network::Regtest),
    );
    assert!(mainnet
        .address(None)
        .unwrap()
        .to_string()
        .starts_with("sp1"));
    let keys = |w: &Wallet| {
        let a = w.address(None).unwrap();
        (a.scan_public_key(), a.spend_public_key())
    };
    assert_ne!(keys(&mainnet), keys(&signet));
    assert_ne!(keys(&mainnet).0, keys(&signet).1);
    assert_ne!(mainnet.scanner().wallet_id(), signet.scanner().wallet_id());
    // Pinned against the BIP32 library directly: coin type 0, not 1.
    use bdk_sp::bitcoin::{
        bip32::{DerivationPath, Xpriv},
        hex::FromHex,
        secp256k1::Secp256k1,
    };
    use std::str::FromStr as _;
    let secp = Secp256k1::new();
    let master = Xpriv::new_master(
        bdk_sp::bitcoin::Network::Bitcoin,
        &Vec::<u8>::from_hex(&seed).unwrap(),
    )
    .unwrap();
    let public = |path: &str| {
        master
            .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
            .unwrap()
            .private_key
            .public_key(&secp)
    };
    assert_eq!(
        keys(&mainnet),
        (public("352h/0h/0h/1h/0"), public("352h/0h/0h/0h/0"))
    );
    assert_eq!(
        keys(&signet),
        (public("352h/1h/0h/1h/0"), public("352h/1h/0h/0h/0"))
    );
    assert_eq!(keys(&regtest), keys(&signet));
    // Destinations are read for the wallet's network only.
    let to_mainnet = mainnet.address(None).unwrap().to_string();
    assert!(Destination::parse(&to_mainnet, Network::Mainnet).is_ok());
    assert!(Destination::parse(&to_mainnet, Network::Signet).is_err());
    assert!(
        Destination::parse(&signet.address(None).unwrap().to_string(), Network::Mainnet).is_err()
    );
    assert!(Destination::parse(
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        Network::Mainnet
    )
    .is_ok());
    assert!(Destination::parse(
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        Network::Signet
    )
    .is_err());
}

/// The wallet signs for its own network only: a source on another chain, or
/// outputs detected on another network, are refused before anything is
/// derived. (Scripts and signatures do not name a network, so a transaction
/// signed for one chain's data would be valid on that chain.)
#[test]
fn nothing_is_signed_for_another_network_than_the_wallets() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let payments = [Payment {
        to: Destination::parse(
            "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
            Network::Regtest,
        )
        .unwrap(),
        amount_sats: 5_000,
    }];
    let build = |source: &TestSource, candidates: &[DetectedOutput]| {
        build_and_sign(
            &wallet,
            source,
            candidates,
            &payments,
            2,
            &SendLimits::default(),
        )
    };
    assert!(build(&source, &candidates).is_ok());

    // The same data, said to be another network's: by the source and the
    // detections together, by the source alone, by one detection alone.
    let elsewhere = silent_sync::ChainIdentity::for_network(Network::Signet);
    let mut relabelled = candidates.clone();
    for d in &mut relabelled {
        d.network = Network::Signet;
    }
    let mut one_relabelled = candidates.clone();
    one_relabelled.last_mut().unwrap().network = Network::Signet;

    source.chain_override.set(Some(elsewhere));
    for candidates in [&relabelled, &candidates] {
        let error = build(&source, candidates).unwrap_err();
        assert!(
            matches!(error, SendError::AnotherNetwork { .. }),
            "{error:?}"
        );
    }
    source.chain_override.set(None);
    for candidates in [&relabelled, &one_relabelled] {
        let error = build(&source, candidates).unwrap_err();
        assert!(
            matches!(error, SendError::AnotherNetwork { .. }),
            "{error:?}"
        );
    }
}

/// "I already have that transaction" is a refusal, but it is not "not
/// sent": such an answer must not let the pending record be taken back.
#[test]
fn a_transaction_the_node_already_has_is_not_classified_as_not_sent() {
    use silent_wallet::RpcError;
    for reason in [
        "txn-already-in-mempool",
        "txn-already-known",
        "Transaction outputs already in utxo set",
        "Transaction already in block chain",
    ] {
        let error = RpcError::Rejected(reason.to_string());
        assert!(error.means_already_with_node(), "{reason}");
        assert!(!error.means_not_done(), "{reason}");
    }
    for reason in [
        "min relay fee not met",
        "bad-txns-inputs-missingorspent",
        "",
    ] {
        let error = RpcError::Rejected(reason.to_string());
        assert!(!error.means_already_with_node(), "{reason}");
        assert!(error.means_not_done(), "{reason}");
    }
    assert!(!RpcError::NoAnswer("timed out".to_string()).means_already_with_node());
}

/// What an observer could otherwise read from a transaction: the change is
/// not in a fixed place, the inputs are not in the wallet's order, and the
/// lock time is the one Bitcoin Core's wallet sets (the tip's height, now
/// and then up to 99 blocks lower) — never 0, and never in the future.
#[test]
fn order_and_lock_time_give_nothing_away() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let tip = source.tip().unwrap().height;
    assert!(tip > 0);
    let largest = candidates.iter().map(|d| d.value_sats).max().unwrap();
    let payments = [Payment {
        to: Destination::parse(
            "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
            Network::Regtest,
        )
        .unwrap(),
        amount_sats: largest + 1_000,
    }];
    let mut change_places = std::collections::BTreeSet::new();
    let mut input_orders = std::collections::BTreeSet::new();
    let mut lock_times = std::collections::BTreeSet::new();
    for _ in 0..200 {
        let built = build_and_sign(
            &wallet,
            &source,
            &candidates,
            &payments,
            2,
            &SendLimits::default(),
        )
        .unwrap();
        change_places.insert(built.change.expect("change").vout);
        assert!(built.inputs.len() >= 2);
        input_orders.insert(built.inputs.iter().map(|i| i.outpoint).collect::<Vec<_>>());
        let lock_time = built.transaction.lock_time;
        assert!(lock_time.is_block_height());
        let height = lock_time.to_consensus_u32();
        assert!(
            height <= tip && height >= tip.saturating_sub(99),
            "{height}"
        );
        lock_times.insert(height);
    }
    // (Each of these fails by chance less often than once in 2^190.)
    assert_eq!(change_places, [0, 1].into());
    assert!(input_orders.len() >= 2);
    // Mostly the tip itself; a lower one in about a tenth of the cases
    // (none in 200 happens by chance less often than once in 10^9). How far
    // lower is checked with a higher tip in `send.rs`.
    assert!(lock_times.contains(&tip));
    assert!(lock_times.len() >= 2, "{lock_times:?}");
}

/// Outputs received under different labels are spent together only when no
/// label's outputs can pay alone, and the result says when that happened.
#[test]
fn labels_are_not_spent_together_unless_it_takes_all() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let mut by_label = std::collections::BTreeMap::<Option<u32>, u64>::new();
    for d in &candidates {
        *by_label.entry(d.label).or_default() += d.value_sats;
    }
    assert!(by_label.len() >= 2, "the fixture pays more than one label");
    let build = |candidates: &[DetectedOutput], amount_sats: u64| {
        build_and_sign(
            &wallet,
            &source,
            candidates,
            &[Payment {
                to: Destination::parse(
                    "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
                    Network::Regtest,
                )
                .unwrap(),
                amount_sats,
            }],
            2,
            &SendLimits::default(),
        )
    };
    let label_of = |outpoint: OutPoint| {
        candidates
            .iter()
            .find(|c| OutPoint::new(c.txid, c.vout) == outpoint)
            .unwrap()
            .label
    };

    // Whichever label stands first among the candidates: a payment that the
    // richest label can make alone spends that label's outputs only, even
    // if outputs of other labels were offered before them.
    let (&rich, &holds) = by_label.iter().max_by_key(|(_, sats)| **sats).unwrap();
    let mut others_first = candidates.clone();
    others_first.sort_by_key(|d| d.label == rich);
    assert_ne!(others_first[0].label, rich);
    let second = by_label
        .iter()
        .filter(|(label, _)| **label != rich)
        .map(|(_, sats)| *sats)
        .max()
        .unwrap();
    assert!(holds > second + 5_000, "one label must hold clearly more");
    for offered in [&candidates, &others_first] {
        let built = build(offered, second + 1_000).unwrap();
        assert!(!built.mixes_labels);
        assert!(built.warnings().is_empty());
        assert!(built.inputs.iter().all(|i| label_of(i.outpoint) == rich));
    }

    // More than any one label holds: everything is used, and it is said.
    let built = build(&candidates, holds + 1_000).unwrap();
    assert!(built.mixes_labels);
    assert!(built.warnings()[0].contains("different labels together"));
    let labels: std::collections::BTreeSet<_> =
        built.inputs.iter().map(|i| label_of(i.outpoint)).collect();
    assert!(labels.len() >= 2);
}

/// When some outputs pay the amount and the fee with (almost) nothing left,
/// those are spent and no change is made — whatever stands before them among
/// the candidates. Nothing in such a transaction leads back to the wallet.
#[test]
fn a_payment_that_some_outputs_cover_exactly_leaves_no_change() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let rate = 2;
    let build = |amount_sats: u64| {
        build_and_sign(
            &wallet,
            &source,
            &candidates,
            &[Payment {
                to: Destination::parse(
                    "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
                    Network::Regtest,
                )
                .unwrap(),
                amount_sats,
            }],
            rate,
            &SendLimits::default(),
        )
    };
    // The last candidate with a value of its own, well above the fee.
    let target = candidates
        .iter()
        .rev()
        .find(|c| {
            c.value_sats > 5_000
                && candidates
                    .iter()
                    .filter(|o| o.value_sats == c.value_sats)
                    .count()
                    == 1
        })
        .unwrap();
    assert_ne!(
        (candidates[0].txid, candidates[0].vout),
        (target.txid, target.vout)
    );
    // The fee of a transaction with one input and one output of this kind.
    let one_in_one_out = {
        let built = build_and_sign(
            &wallet,
            &source,
            std::slice::from_ref(target),
            &[Payment {
                to: Destination::parse(
                    "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
                    Network::Regtest,
                )
                .unwrap(),
                amount_sats: target.value_sats - 400,
            }],
            rate,
            &SendLimits::default(),
        )
        .unwrap();
        assert_eq!(built.change, None);
        built.vsize * rate
    };
    for spare in [0, 40] {
        let built = build(target.value_sats - one_in_one_out - spare).unwrap();
        assert_eq!(built.change, None);
        assert_eq!(built.inputs.len(), 1);
        assert_eq!(
            built.inputs[0].outpoint,
            OutPoint::new(target.txid, target.vout)
        );
        assert_eq!(built.fee_sats, one_in_one_out + spare);
        assert_eq!(built.transaction.output.len(), 1);
    }

    // At a high fee rate a change output costs more — to make now and to
    // spend later — than a few thousand satoshi are worth: they become fee.
    // Unless that takes the fee above a limit; then change is made after all.
    let rich = candidates.iter().max_by_key(|c| c.value_sats).unwrap();
    let high = 50;
    let base = one_in_one_out / rate * high;
    let spare = 4_000;
    let build = |limits: SendLimits| {
        build_and_sign(
            &wallet,
            &source,
            std::slice::from_ref(rich),
            &[Payment {
                to: Destination::parse(
                    "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
                    Network::Regtest,
                )
                .unwrap(),
                amount_sats: rich.value_sats - base - spare,
            }],
            high,
            &limits,
        )
        .unwrap()
    };
    let built = build(SendLimits::default());
    assert_eq!((built.change.clone(), built.fee_sats), (None, base + spare));
    let built = build(SendLimits::default().with_max_fee_sats(base + 3_000));
    let change = built
        .change
        .expect("change, because the fee limit asks for it");
    assert_eq!(built.fee_sats, base + 43 * high);
    assert_eq!(change.amount_sats, spare - 43 * high);
    // The same with the limit as a share of the amount.
    let amount = rich.value_sats - base - spare;
    let percent = (base + 3_000) * 100 / amount;
    assert!((base + spare) * 100 > percent * amount);
    assert!((base + 43 * high) * 100 <= percent * amount);
    let built = build(SendLimits::default().with_max_fee_percent(percent));
    let change = built
        .change
        .expect("change, because the fee limit asks for it");
    assert_eq!(built.fee_sats, base + 43 * high);
    assert_eq!(change.amount_sats, spare - 43 * high);
}

/// Outputs of two labels that together pay without change are not taken
/// while one label can pay alone (with change): leaving no change is not
/// worth showing that the labels belong to one wallet.
#[test]
fn no_change_is_not_bought_by_spending_labels_together() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let rate = 2;
    let to = Destination::parse(
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        Network::Regtest,
    )
    .unwrap();
    let build = |candidates: &[DetectedOutput], amount_sats: u64| {
        build_and_sign(
            &wallet,
            &source,
            candidates,
            &[Payment {
                to: to.clone(),
                amount_sats,
            }],
            rate,
            &SendLimits::default(),
        )
    };
    // Two outputs of different labels, each the only one of its value.
    let unique = |d: &&DetectedOutput| {
        d.value_sats > 5_000
            && candidates
                .iter()
                .filter(|o| o.value_sats == d.value_sats)
                .count()
                == 1
    };
    let first = candidates
        .iter()
        .filter(unique)
        .find(|d| d.label.is_some())
        .unwrap();
    let second = candidates
        .iter()
        .filter(unique)
        .find(|d| d.label != first.label && d.label.is_some())
        .unwrap();
    let pair = [first.clone(), second.clone()];
    let sum = first.value_sats + second.value_sats;
    // The fee of the two of them paying one output, nothing left over.
    let fee = {
        let built = build(&pair, sum - 600).unwrap();
        assert_eq!(built.change, None);
        built.vsize * rate
    };
    let exact = build(&pair, sum - fee).unwrap();
    assert!(exact.change.is_none() && exact.mixes_labels);

    // With everything offered, a label that pays alone is used instead.
    let mut alone = std::collections::BTreeMap::<Option<u32>, u64>::new();
    for d in &candidates {
        *alone.entry(d.label).or_default() += d.value_sats;
    }
    assert!(alone.values().any(|sats| *sats > sum + 5_000));
    let built = build(&candidates, sum - fee).unwrap();
    assert!(!built.mixes_labels);
    let labels: std::collections::BTreeSet<_> = built
        .inputs
        .iter()
        .map(|i| {
            candidates
                .iter()
                .find(|c| OutPoint::new(c.txid, c.vout) == i.outpoint)
                .unwrap()
                .label
        })
        .collect();
    assert_eq!(labels.len(), 1);
}

/// An output the user set aside is not among the unspent ones — so it is
/// not offered to a transaction — until it is released; the list survives
/// being written and read, and an older record without one still reads.
#[test]
fn outputs_held_back_are_listed_apart_and_not_offered() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let all = holdings(&store, 1, &PendingSpends::default()).unwrap();
    assert!(all.held_back.is_empty());
    let apart = all.unspent[0].output.clone();
    let outpoint = OutPoint::new(apart.txid, apart.vout);

    let mut record = PendingSpends::default();
    assert!(record.hold_back(outpoint));
    assert!(!record.hold_back(outpoint));
    let dir = std::env::temp_dir().join(format!("silent-wallet-held-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("pending.json");
    record.save(&file).unwrap();
    let record = PendingSpends::load(&file).unwrap();
    assert_eq!(
        record.held_back().iter().collect::<Vec<_>>(),
        vec![&outpoint]
    );

    let held = holdings(&store, 1, &record).unwrap();
    assert_eq!(held.held_back.len(), 1);
    assert_eq!(held.held_back[0].output, apart);
    assert_eq!(held.unspent.len(), all.unspent.len() - 1);
    assert_eq!(held.unspent_sats(), all.unspent_sats() - apart.value_sats);
    assert!(held.unspent.iter().all(|h| h.output != apart));

    // A transaction built before the hold does not get to spend it: the
    // check made right before sending finds it, among other inputs too.
    let other = all.unspent[1].output.clone();
    let other = OutPoint::new(other.txid, other.vout);
    assert_eq!(
        record.first_unavailable([&other, &outpoint]),
        Some(outpoint)
    );
    assert_eq!(record.first_unavailable([&other]), None);

    let mut record = record;
    assert!(record.release(outpoint));
    assert!(!record.release(outpoint));
    assert_eq!(holdings(&store, 1, &record).unwrap(), all);
    assert_eq!(record.first_unavailable([&other, &outpoint]), None);
    // Nor does it spend what a transaction sent meanwhile spends.
    record.record(PendingTransaction {
        txid: apart.txid,
        inputs: vec![other],
        sent_at_height: 8,
        confirmed_at: None,
    });
    assert_eq!(record.first_unavailable([&outpoint, &other]), Some(other));

    // What can be held: an output that would otherwise be spent, or one held
    // already. Not one being spent, and not one that is not known.
    let now = holdings(&store, 1, &record).unwrap();
    assert!(now.can_hold(outpoint));
    assert!(!now.can_hold(other));
    assert!(!now.can_hold(OutPoint::new(apart.txid, 999)));
    record.hold_back(outpoint);
    assert!(holdings(&store, 1, &record).unwrap().can_hold(outpoint));
    // One with too few confirmations yet can be held too.
    let young = holdings(&store, 1_000, &PendingSpends::default()).unwrap();
    assert!(young.unspent.is_empty());
    assert!(young.can_hold(outpoint));

    // A record written before there was such a list.
    std::fs::write(&file, r#"{"transactions":[]}"#).unwrap();
    assert!(PendingSpends::load(&file).unwrap().held_back().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A label that can pay alone is used alone, even when its oldest outputs
/// would take more inputs than allowed, or a fee above the limits: then its
/// other outputs, or another label, are tried before labels are mixed.
#[test]
fn a_label_that_can_pay_alone_is_found_past_its_oldest_outputs() {
    let source = TestSource::new();
    let wallet = wallet();
    let store = synced(&source, &wallet);
    let candidates = unspent(&store);
    let to = Destination::parse(
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        Network::Regtest,
    )
    .unwrap();
    let build = |candidates: &[DetectedOutput], amount_sats: u64, rate, limits: &SendLimits| {
        build_and_sign(
            &wallet,
            &source,
            candidates,
            &[Payment {
                to: to.clone(),
                amount_sats,
            }],
            rate,
            limits,
        )
    };
    let find = |sats: u64, label: Option<u32>| {
        candidates
            .iter()
            .find(|c| c.value_sats == sats && c.label == label)
            .unwrap_or_else(|| panic!("the fixture has {sats} sat under {label:?}"))
            .clone()
    };
    let label_of = |outpoint: OutPoint| {
        candidates
            .iter()
            .find(|c| OutPoint::new(c.txid, c.vout) == outpoint)
            .unwrap()
            .label
    };
    let (a15, a3) = (find(15_000, Some(0)), find(3_000, Some(0)));
    let (n10, n20, n50) = (find(10_000, None), find(20_000, None), find(50_000, None));

    // At most two inputs. No label's two oldest outputs pay 31,000 sat, but
    // its newest one does: that one is spent, nothing else.
    let mut two = SendLimits::default();
    two.max_inputs = 2;
    let offered = [
        a15.clone(),
        n20.clone(),
        n10.clone(),
        n50.clone(),
        a3.clone(),
    ];
    let built = build(&offered, 31_000, 2, &two).unwrap();
    assert!(!built.mixes_labels);
    assert_eq!(
        built.inputs.iter().map(|i| i.outpoint).collect::<Vec<_>>(),
        vec![OutPoint::new(n50.txid, n50.vout)]
    );

    // The first label pays only with a fee above the limit (what is left is
    // too little for change): the next label is used.
    let pair = [a3.clone(), a15.clone()];
    let fee = {
        let built = build(&pair, 18_000 - 450, 1, &SendLimits::default()).unwrap();
        assert_eq!(built.change, None);
        built.vsize
    };
    let amount = 18_000 - fee - 200;
    let limits = SendLimits::default().with_max_fee_sats(fee + 150);
    assert!(build(&pair, amount, 1, &limits).is_err());
    let offered = [a3, a15, n20, n10, n50];
    let built = build(&offered, amount, 1, &limits).unwrap();
    assert!(!built.mixes_labels);
    assert!(built.inputs.iter().all(|i| label_of(i.outpoint).is_none()));
    assert!(built.fee_sats <= fee + 150);

    // One output of a label pays, but leaves too little for change: all of
    // it would be fee, above the limit. With one more output of the same
    // label there is change, and the fee is within the limit.
    let a15 = find(15_000, Some(0));
    let a3 = find(3_000, Some(0));
    let one = {
        let built = build(
            std::slice::from_ref(&a15),
            15_000 - 450,
            1,
            &SendLimits::default(),
        )
        .unwrap();
        assert_eq!(built.change, None);
        built.vsize
    };
    let amount = 15_000 - one - 200;
    let limits = SendLimits::default().with_max_fee_sats(one + 150);
    assert!(build(std::slice::from_ref(&a15), amount, 1, &limits).is_err());
    let offered = [a15, a3, find(7_000, Some(1))];
    let built = build(&offered, amount, 1, &limits).unwrap();
    assert!(!built.mixes_labels);
    assert_eq!(built.inputs.len(), 2);
    assert!(built.change.is_some());
    assert!(built.fee_sats <= one + 150);

    // What covers the amount but not the fee at the rate asked for is not
    // enough: nothing is built that pays less than the rate.
    for spare in [0, 50, one - 1] {
        let short = build(&offered[..1], 15_000 - spare, 1, &SendLimits::default());
        assert!(
            matches!(short, Err(SendError::InsufficientFunds { .. })),
            "{spare}: {short:?}"
        );
    }
}
