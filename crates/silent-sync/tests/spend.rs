//! Feature `spend`: deriving again, from a full block, what a wallet needs to
//! spend a detected output — and reading addresses.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    fixture_gen::{self, LABEL_USED, LAB_RECEIVER},
    test_source::TestSource,
};
use silent_sync::{
    bitcoin::{
        hashes::Hash as _,
        secp256k1::{Scalar, Secp256k1},
        Txid,
    },
    engine::scan_range,
    models::EvidenceKind,
    network::{ChainIdentity, Network},
    protocol::{ProtocolError, ReceiveScanner, SilentPaymentAddress},
    source::{
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
    },
    spend::{recover_spendable, SpendError},
    DetectedOutput,
};

fn scanner() -> ReceiveScanner {
    ReceiveScanner::new(&fixture_gen::lab_receiver_keys(), &[LABEL_USED]).unwrap()
}

fn detections(source: &TestSource) -> Vec<DetectedOutput> {
    scan_range(
        source.main_chain(),
        &scanner(),
        ChainIdentity::for_network(Network::Regtest),
        1,
        8,
    )
    .unwrap()
    .detected_received_outputs
}

/// For every detected output the recovered tweak, added to the spend private
/// key (published for this test receiver), is the private key of exactly
/// that output — labelled or not.
#[test]
fn the_recovered_tweak_opens_the_detected_output() {
    let source = TestSource::new();
    let secp = Secp256k1::new();
    let (_, spend_sk) = fixture_gen::vector_secret_keys(LAB_RECEIVER.0, LAB_RECEIVER.1);
    let found = detections(&source);
    assert_eq!(found.len(), 7);
    assert!(found.iter().any(|d| d.label == Some(LABEL_USED)));
    assert!(found.iter().any(|d| d.label.is_none()));

    for d in &found {
        let spendable = recover_spendable(&source, &scanner(), d).unwrap();
        assert_eq!(
            (spendable.outpoint.txid, spendable.outpoint.vout),
            (d.txid, d.vout)
        );
        assert_eq!(spendable.txout.value.to_sat(), d.value_sats);
        assert_eq!(spendable.label, d.label);
        assert_eq!(spendable.block, d.block);
        let output_sk = spend_sk
            .add_tweak(&Scalar::from(spendable.sensitive_tweak()))
            .unwrap();
        let (output_key, _) = output_sk.x_only_public_key(&secp);
        assert!(spendable.txout.script_pubkey.is_p2tr());
        assert_eq!(
            spendable.txout.script_pubkey.as_bytes()[2..],
            output_key.serialize(),
            "{}:{}",
            d.txid,
            d.vout
        );
        // The tweak is not printed.
        let shown = format!("{spendable:?}");
        assert!(shown.contains("<redacted>"));
        assert!(!shown.contains(&spendable.sensitive_tweak().display_secret().to_string()));
    }
}

/// Anything that is not exactly the detected output, on full blocks, is an
/// error: no key is derived from a claim or from another block.
#[test]
fn nothing_is_recovered_unless_everything_matches() {
    let source = TestSource::new();
    let found = detections(&source);
    let d = found.iter().find(|d| d.block.height >= 7).unwrap().clone();
    let recover = |source: &dyn silent_sync::ScanSource, d: &DetectedOutput| {
        recover_spendable(source, &scanner(), d).map(|_| ())
    };
    assert_eq!(recover(&source, &d), Ok(()));

    // A detection that rests on an index's claim.
    let mut claimed = d.clone();
    claimed.provenance.evidence_kind = EvidenceKind::TweakIndex;
    assert!(matches!(
        recover(&source, &claimed),
        Err(SpendError::DetectionNotFromFullBlocks {
            found: "tweak_index",
            ..
        })
    ));
    // A source that is an index.
    let index = LocalTweakIndex::new(
        FixtureSource::from_file(fixture_gen::generate()).unwrap(),
        IndexPolicy::Bip352,
    );
    assert!(matches!(
        recover(&index, &d),
        Err(SpendError::NotFullBlocks { .. })
    ));
    // Another network.
    source
        .chain_override
        .set(Some(ChainIdentity::for_network(Network::Signet)));
    assert!(matches!(
        recover(&source, &d),
        Err(SpendError::NetworkMismatch { .. })
    ));
    source.chain_override.set(None);
    // The chain was reorganized: another block at that height.
    source.on_fork.set(true);
    assert_eq!(
        recover(&source, &d),
        Err(SpendError::BlockChanged {
            height: d.block.height
        })
    );
    source.on_fork.set(false);
    // The source is behind.
    source.visible_tip.set(Some(d.block.height - 1));
    assert!(matches!(recover(&source, &d), Err(SpendError::Scan(_))));
    source.visible_tip.set(None);

    // Not what was detected: another transaction, output, amount or label.
    let mut other_tx = d.clone();
    other_tx.txid = Txid::from_byte_array([4; 32]);
    assert!(matches!(
        recover(&source, &other_tx),
        Err(SpendError::TransactionMissing { .. })
    ));
    let mut other_output = d.clone();
    other_output.vout += 30;
    let mut other_amount = d.clone();
    other_amount.value_sats += 1;
    let mut other_label = d.clone();
    other_label.label = Some(d.label.map_or(LABEL_USED, |m| m + 1));
    for altered in [other_output, other_amount, other_label] {
        assert!(matches!(
            recover(&source, &altered),
            Err(SpendError::NotReproduced { .. })
        ));
    }
    // A scanner for someone else finds nothing to spend.
    let other = ReceiveScanner::new(&fixture_gen::other_receiver_keys(), &[]).unwrap();
    assert!(matches!(
        recover_spendable(&source, &other, &d),
        Err(SpendError::NotReproduced { .. })
    ));
}

/// Addresses are read back from their text form for the network they are
/// for, and for no other. A mainnet address is refused whatever is asked.
#[test]
fn addresses_are_read_for_their_own_network_only() {
    let keys = fixture_gen::lab_receiver_keys();
    for network in [Network::Regtest, Network::Signet] {
        for address in [
            keys.address(network),
            keys.labelled_address(network, LABEL_USED).unwrap(),
        ] {
            let text = address.to_string();
            assert_eq!(SilentPaymentAddress::parse(&text, network), Ok(address));
            let other = match network {
                Network::Regtest => Network::Signet,
                _ => Network::Regtest,
            };
            assert!(matches!(
                SilentPaymentAddress::parse(&text, other),
                Err(ProtocolError::Address(reason)) if reason.contains("is not a")
            ));
        }
    }
    let mainnet = bdk_sp::encoding::SilentPaymentCode::new_v0(
        keys.scan_public_key(),
        keys.spend_public_key(),
        bdk_sp::bitcoin::Network::Bitcoin,
    )
    .to_string();
    assert!(mainnet.starts_with("sp1"));
    for network in [Network::Regtest, Network::Signet] {
        assert!(SilentPaymentAddress::parse(&mainnet, network).is_err());
    }
    // With the `mainnet` feature the same text is read for mainnet, and for
    // mainnet only; what this crate prints for mainnet is that very text.
    #[cfg(feature = "mainnet")]
    {
        let address = SilentPaymentAddress::parse(&mainnet, Network::Mainnet).unwrap();
        assert_eq!(address, keys.address(Network::Mainnet));
        assert_eq!(address.to_string(), mainnet);
        assert!(address.network().is_mainnet());
        for network in [Network::Regtest, Network::Signet] {
            let text = keys.address(network).to_string();
            assert!(SilentPaymentAddress::parse(&text, Network::Mainnet).is_err());
        }
    }
    for garbage in [
        "",
        "sprt1",
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        "hello",
    ] {
        assert!(matches!(
            SilentPaymentAddress::parse(garbage, Network::Regtest),
            Err(ProtocolError::Address(_))
        ));
    }
}
