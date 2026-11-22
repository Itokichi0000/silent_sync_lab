//! The one-stop interface (`silent_wallet::client`): seed phrases, and a
//! wallet opened, synced and — on a real regtest node — paid from.
#![cfg(feature = "client")]
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fake_http::FakeCoreRest,
    fixture_gen,
    regtest_node::Node,
    regtest_wallet::{signed_transaction, silent_payment_scripts, Faucet, InputKind},
};
use silent_sync::Network;
use silent_wallet::client::{ClientConfig, ClientError, SeedPhrase, WalletClient, WordCount};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "silent-wallet-client-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The phrase is BIP39: the published test vector for all-zero entropy gives
/// the published words and, with the passphrase "TREZOR", the published
/// seed — checked here through the keys derived from it.
#[test]
fn seed_phrases_are_bip39_and_are_never_shown_unasked() {
    use bdk_sp::bitcoin::{
        bip32::{DerivationPath, Xpriv},
        hex::FromHex,
        secp256k1::Secp256k1,
    };
    use std::str::FromStr as _;

    let phrase = SeedPhrase::from_entropy(&[0u8; 16]).unwrap();
    let words = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                 abandon about";
    assert_eq!(phrase.reveal(), words);
    let seed = Vec::<u8>::from_hex(
        "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e53495531f09a6987599d18264c1e1c9\
         2f2cf141630c7a3c4ab7c81b2f001698e7463b04",
    )
    .unwrap();
    let secp = Secp256k1::new();
    let master = Xpriv::new_master(bdk_sp::bitcoin::Network::Regtest, &seed).unwrap();
    let public = |path: &str| {
        master
            .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
            .unwrap()
            .private_key
            .public_key(&secp)
    };
    let address = |passphrase: &str| {
        phrase
            .keys(passphrase, Network::Regtest)
            .unwrap()
            .receiver()
            .address(Network::Regtest)
    };
    let trezor = address("TREZOR");
    assert_eq!(trezor.scan_public_key(), public("352h/1h/0h/1h/0"));
    assert_eq!(trezor.spend_public_key(), public("352h/1h/0h/0h/0"));
    // The passphrase is part of the wallet.
    assert_ne!(address(""), trezor);

    // The 24-word vector (all-zero entropy, passphrase "TREZOR").
    let long = SeedPhrase::from_entropy(&[0u8; 32]).unwrap();
    assert_eq!(long.reveal(), format!("{}art", "abandon ".repeat(23)));
    let seed = Vec::<u8>::from_hex(
        "bda85446c68413707090a52022edd26a1c9462295029f2e60cd7c4f2bbd3097170af7a4d73245cafa9c3cca8\
         d561a7c3de6f5d4a10be8ed2a5e608d68f92fcc8",
    )
    .unwrap();
    let master = Xpriv::new_master(bdk_sp::bitcoin::Network::Regtest, &seed).unwrap();
    let expected = master
        .derive_priv(&secp, &DerivationPath::from_str("352h/1h/0h/1h/0").unwrap())
        .unwrap()
        .private_key
        .public_key(&secp);
    let derived = long
        .keys("TREZOR", Network::Regtest)
        .unwrap()
        .receiver()
        .address(Network::Regtest);
    assert_eq!(derived.scan_public_key(), expected);

    // Reading a phrase back: spacing and case as a user types them.
    let typed = format!("  {}  ", words.to_uppercase().replace(' ', "   "));
    assert_eq!(SeedPhrase::parse(&typed).unwrap().reveal(), words);

    // New phrases: the asked length, different every time, valid.
    for (count, n) in [(WordCount::Twelve, 12), (WordCount::TwentyFour, 24)] {
        let (a, b) = (
            SeedPhrase::generate(count).unwrap(),
            SeedPhrase::generate(count).unwrap(),
        );
        assert_eq!(a.reveal().split(' ').count(), n);
        assert_ne!(a.reveal(), b.reveal());
        assert_eq!(SeedPhrase::parse(&a.reveal()).unwrap().reveal(), a.reveal());
        // Not in `Debug`, and there is no `Display`.
        let shown = format!("{a:?}");
        assert_eq!(shown, "SeedPhrase(<redacted>)");
        assert!(a
            .reveal()
            .split(' ')
            .all(|word| word.len() < 3 || !shown.contains(word)));
    }

    // Refused: a wrong checksum, a word that is not in the list, lengths
    // this wallet does not use (15 words are valid BIP39), entropy of
    // another size. The refusal never repeats what was typed.
    let fifteen =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
                   abandon abandon abandon abandon address";
    for bad in [
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
         abandon",
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon \
         zzzzzz",
        fifteen,
        "",
    ] {
        let error = SeedPhrase::parse(bad).unwrap_err();
        assert!(matches!(error, ClientError::InvalidSeedPhrase), "{bad}");
        let text = format!("{error} {error:?}");
        assert!(
            !text.contains("abandon") && !text.contains("zzzzzz"),
            "{text}"
        );
    }
    for size in [0, 15, 20, 31, 33, 64] {
        assert!(
            SeedPhrase::from_entropy(&vec![7u8; size]).is_err(),
            "{size}"
        );
    }
}

/// Opening a wallet: the store is created once, bound to the phrase, the
/// network and the labels, and nothing is created or changed when one of
/// them does not fit.
#[test]
fn a_wallet_is_opened_with_a_phrase_and_bound_to_it() {
    let dir = scratch("offline");
    let db = dir.join("wallet.sqlite3");
    let node = FakeCoreRest::serve(&fixture_gen::generate());
    let phrase = SeedPhrase::generate(WordCount::Twelve).unwrap();
    let config = || ClientConfig::new(Network::Regtest, &db, node.url());

    // The node is a regtest node: no store for another network is made.
    let signet = ClientConfig::new(Network::Signet, &db, node.url());
    assert!(matches!(
        WalletClient::connect(signet, &phrase, ""),
        Err(ClientError::NodeOnAnotherNetwork {
            node: Network::Regtest,
            asked: Network::Signet
        })
    ));
    assert!(!db.exists());
    // Nor one for mainnet, whatever this build can name.
    if let Ok(mainnet) = "mainnet".parse::<Network>() {
        assert!(
            WalletClient::connect(ClientConfig::new(mainnet, &db, node.url()), &phrase, "")
                .is_err()
        );
        assert!(!db.exists());
    }

    let mut wallet = WalletClient::connect(config().with_labels(&[2]), &phrase, "").unwrap();
    assert_eq!(wallet.network(), Network::Regtest);
    assert_eq!(wallet.store_meta().birthday, 8);
    assert_eq!(wallet.store_meta().labels, vec![0, 2]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let address = wallet.receive_address(None).unwrap().to_string();
    assert!(address.starts_with("sprt1"), "{address}");
    assert_ne!(
        wallet.receive_address(Some(2)).unwrap().to_string(),
        address
    );
    // Not handed out: the change label, and a label the store does not scan.
    assert!(wallet.receive_address(Some(0)).is_err());
    assert!(matches!(
        wallet.receive_address(Some(3)),
        Err(ClientError::LabelsDiffer { .. })
    ));

    // Nothing was scanned yet; after a sync the store is at the tip, empty.
    assert!(wallet.holdings().unwrap().as_of.is_none());
    let summary = wallet.sync().unwrap();
    assert!(summary.report.caught_up && summary.settled.is_empty());
    assert_eq!(summary.report.blocks_scanned, 1);
    let held = wallet.holdings().unwrap();
    assert_eq!(held.as_of.map(|b| b.height), Some(8));
    assert_eq!(held.unspent_sats(), 0);
    assert!(wallet.received().unwrap().is_empty() && wallet.pending().unwrap().is_empty());

    // An amount above the limit is refused before anything else happens;
    // within it, the empty wallet has nothing to pay with. Nothing is sent
    // either way, and nothing is recorded as pending.
    let mut limited =
        WalletClient::connect(config().with_max_amount_sats(5_000), &phrase, "").unwrap();
    assert!(matches!(
        limited.prepare_send(&address, 5_001, 1),
        Err(ClientError::AmountAboveLimit {
            amount_sats: 5_001,
            limit_sats: 5_000,
            ..
        })
    ));
    let nothing = limited.prepare_send(&address, 5_000, 1).unwrap_err();
    assert!(matches!(nothing, ClientError::Build(_)), "{nothing:?}");
    assert!(nothing.to_string().ends_with("nothing was sent"));
    assert!(matches!(
        wallet.prepare_send("not an address", 1_000, 1),
        Err(ClientError::Build(_))
    ));
    assert!(wallet.pending().unwrap().is_empty());
    drop((wallet, limited));

    // Opened again with the same phrase; not with another phrase, another
    // passphrase, or other labels.
    let again = WalletClient::connect(config(), &phrase, "").unwrap();
    assert_eq!(again.receive_address(None).unwrap().to_string(), address);
    assert_eq!(again.holdings().unwrap().as_of.map(|b| b.height), Some(8));
    drop(again);
    let other = SeedPhrase::generate(WordCount::Twelve).unwrap();
    assert!(matches!(
        WalletClient::connect(config(), &other, ""),
        Err(ClientError::WrongSeed)
    ));
    assert!(matches!(
        WalletClient::connect(config(), &phrase, "another passphrase"),
        Err(ClientError::WrongSeed)
    ));
    assert!(matches!(
        WalletClient::connect(config().with_labels(&[2, 3]), &phrase, ""),
        Err(ClientError::LabelsDiffer { .. })
    ));
    assert!(WalletClient::connect(config().with_labels(&[2]), &phrase, "").is_ok());
    // (The change label may be named; it is scanned in any case.)
    assert!(WalletClient::connect(config().with_labels(&[0, 2, 2]), &phrase, "").is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two wallets on a real regtest node: one is paid, pays the other — a
/// silent payment address and an ordinary address — and both see it after
/// the block.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn receive_and_send_between_two_wallets_on_a_node() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let dir = scratch("node");
    let config = |name: &str| {
        ClientConfig::new(Network::Regtest, dir.join(name), node.rest_url())
            .with_rpc_cookie_file(node.cookie_file())
    };
    let phrase_a = SeedPhrase::generate(WordCount::Twelve).unwrap();
    let phrase_b = SeedPhrase::generate(WordCount::TwentyFour).unwrap();
    let mut alice = WalletClient::connect(config("alice.sqlite3"), &phrase_a, "").unwrap();
    let mut bob = WalletClient::connect(config("bob.sqlite3"), &phrase_b, "pass").unwrap();
    let to_alice = alice.receive_address(None).unwrap();
    let to_bob = bob.receive_address(None).unwrap().to_string();
    let mempool = || node.cli_json(&["getrawmempool"]).as_array().unwrap().len();

    // Two payments to Alice, in two blocks.
    let mut total = 0;
    for funded in faucet.fund(&[InputKind::P2wpkh, InputKind::P2trKeyPath]) {
        let amount = funded.prevout.value.to_sat() - 1_000;
        let scripts = silent_payment_scripts(std::slice::from_ref(&funded), &[to_alice]);
        let tx = signed_transaction(
            std::slice::from_ref(&funded),
            vec![(amount, scripts[0].clone())],
        );
        faucet.broadcast_and_mine(&tx);
        total += amount;
    }
    let summary = alice.sync().unwrap();
    assert!(summary.report.caught_up);
    assert_eq!(summary.report.new_detections.len(), 2);
    let held = alice.holdings().unwrap();
    assert_eq!((held.unspent.len(), held.unspent_sats()), (2, total));
    let largest = held
        .unspent
        .iter()
        .map(|h| h.output.value_sats)
        .max()
        .unwrap();

    // Prepared, not sent: nothing reaches the node, nothing is pending.
    let amount = largest + 10_000; // needs both outputs
    let prepared = alice.prepare_send(&to_bob, amount, 2).unwrap();
    let shown = prepared.to_string();
    assert!(
        shown.contains(&format!("pays        : {amount} sat to {to_bob}"))
            && shown.contains("spends      : 2 outputs")
            && shown.contains(&format!("fee         : {} sat", prepared.fee_sats()))
            && shown.contains("network     : regtest"),
        "{shown}"
    );
    assert_eq!(mempool(), 0);
    assert!(alice.pending().unwrap().is_empty());

    // A wallet without the node's cookie prepares but cannot send.
    let no_cookie = ClientConfig::new(Network::Regtest, dir.join("alice.sqlite3"), node.rest_url());
    let mut cannot = WalletClient::connect(no_cookie, &phrase_a, "").unwrap();
    let again = cannot.prepare_send(&to_bob, amount, 2).unwrap();
    assert!(matches!(cannot.send(again), Err(ClientError::NoRpc)));
    assert_eq!(mempool(), 0);
    assert!(alice.pending().unwrap().is_empty());
    drop(cannot);

    // A transaction prepared by one wallet is not sent through another,
    // although both are at the same block: it would go out while the wallet
    // it spends from keeps no record of it.
    bob.sync().unwrap();
    let alices = alice.prepare_send(&to_bob, amount, 2).unwrap();
    assert!(matches!(bob.send(alices), Err(ClientError::Stale { .. })));
    assert_eq!(mempool(), 0);
    assert!(bob.pending().unwrap().is_empty());

    // A transaction prepared before the chain moved is not sent.
    let stale = alice.prepare_send(&to_bob, amount, 2).unwrap();
    faucet.mine_block(&[]);
    alice.sync().unwrap();
    assert!(matches!(alice.send(stale), Err(ClientError::Stale { .. })));
    assert_eq!(mempool(), 0);

    // Sent: in the node's mempool, on record as pending, its inputs taken.
    let prepared = alice.prepare_send(&to_bob, amount, 2).unwrap();
    let (txid, fee) = (prepared.txid(), prepared.fee_sats());
    let change = prepared
        .details()
        .change
        .as_ref()
        .map(|c| c.amount_sats)
        .unwrap();
    assert_eq!(total, amount + fee + change);
    let twin = prepared.clone();
    assert_eq!(alice.send(prepared).unwrap(), txid);
    assert_eq!(mempool(), 1);
    assert_eq!(alice.pending().unwrap().len(), 1);
    let held = alice.holdings().unwrap();
    assert_eq!((held.unspent_sats(), held.spent_by_pending.len()), (0, 2));
    // The same transaction is not handed over twice, and nothing can be
    // built from inputs that are on their way.
    assert!(matches!(
        alice.send(twin.clone()),
        Err(ClientError::Stale { .. })
    ));
    // Forgotten here while the node still has it: sending it again is not
    // reported as "nothing was sent", and it is back on record.
    assert!(alice.forget_pending(txid).unwrap());
    assert!(alice.pending().unwrap().is_empty());
    let again = alice.send(twin).unwrap_err();
    assert!(
        matches!(again, ClientError::AlreadyWithNode { txid: t } if t == txid),
        "{again:?}"
    );
    assert!(!again.to_string().contains("nothing was sent"));
    assert_eq!((mempool(), alice.pending().unwrap().len()), (1, 1));
    assert!(matches!(
        alice.prepare_send(&to_bob, 1_000, 2),
        Err(ClientError::Build(_))
    ));

    // After the block: Bob holds the payment, Alice her change.
    node.generate_to(1, faucet.miner_address());
    let summary = alice.sync().unwrap();
    assert_eq!(summary.settled.len(), 1);
    assert_eq!(summary.report.new_spends.len(), 2);
    assert_eq!(alice.holdings().unwrap().unspent_sats(), change);
    assert_eq!(alice.spends().unwrap().len(), 2);
    bob.sync().unwrap();
    let held = bob.holdings().unwrap();
    assert_eq!((held.unspent.len(), held.unspent_sats()), (1, amount));
    assert_eq!(held.unspent[0].output.txid, txid);

    // Bob pays an ordinary on-chain address.
    let ordinary = faucet.miner_address().to_string();
    let prepared = bob.prepare_send(&ordinary, 20_000, 3).unwrap();
    assert!(prepared.to_string().contains(&ordinary));
    let txid = bob.send(prepared).unwrap();
    assert_eq!(
        node.cli_json(&["getrawmempool"])[0].as_str(),
        Some(txid.to_string().as_str())
    );
    node.generate_to(1, &ordinary);
    let summary = bob.sync().unwrap();
    assert_eq!(summary.settled.len(), 1);
    assert!(bob
        .pending()
        .unwrap()
        .iter()
        .all(|t| t.confirmed_at.is_some()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build with the `mainnet` feature, on a node with mainnet parameters
/// that has no network access and holds only the genesis block: the wallet
/// opens on mainnet with a mainnet address, a test-network wallet does not
/// open on that node, and the default amount limit applies.
#[cfg(feature = "mainnet")]
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn a_mainnet_wallet_opens_on_a_mainnet_node() {
    use lab_testkit::regtest_node::NodeChain;
    use silent_wallet::client::MAINNET_DEFAULT_MAX_AMOUNT_SATS;

    let node = Node::start(NodeChain::OfflineMainnet);
    assert_eq!(node.cli_json(&["getnetworkinfo"])["networkactive"], false);
    let dir = scratch("mainnet");
    let phrase = SeedPhrase::generate(WordCount::Twelve).unwrap();
    let on = |network, name: &str| {
        WalletClient::connect(
            ClientConfig::new(network, dir.join(name), node.rest_url())
                .with_rpc_cookie_file(node.cookie_file()),
            &phrase,
            "",
        )
    };
    assert!(matches!(
        on(Network::Signet, "signet.sqlite3"),
        Err(ClientError::NodeOnAnotherNetwork { .. })
    ));
    assert!(!dir.join("signet.sqlite3").exists());

    let mut wallet = on(Network::Mainnet, "mainnet.sqlite3").unwrap();
    assert!(wallet.network().is_mainnet());
    let address = wallet.receive_address(None).unwrap().to_string();
    assert!(address.starts_with("sp1"), "{address}");
    assert_eq!(wallet.store_meta().birthday, 1);
    assert_eq!(
        wallet.store_meta().chain.genesis_hash(),
        Network::Mainnet.genesis_hash()
    );
    // Nothing above the genesis block: nothing to scan, nothing held.
    let summary = wallet.sync().unwrap();
    assert_eq!(summary.report.blocks_scanned, 0);
    assert_eq!(wallet.holdings().unwrap().unspent_sats(), 0);
    assert!(matches!(
        wallet.prepare_send(&address, MAINNET_DEFAULT_MAX_AMOUNT_SATS + 1, 1),
        Err(ClientError::AmountAboveLimit {
            limit_sats: MAINNET_DEFAULT_MAX_AMOUNT_SATS,
            ..
        })
    ));
    let _ = std::fs::remove_dir_all(&dir);
}
