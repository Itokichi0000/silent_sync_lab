//! The wallet's command line without a real node: keys from the environment,
//! refusals, and an empty wallet against an in-process fake of Core's REST
//! interface. Sending is covered against a real node in `regtest_wallet.rs`.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report
#![allow(non_snake_case)] // `SEED` is a per-test value used like a constant

use lab_testkit::{fake_http::FakeCoreRest, fixture_gen};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_silent-wallet");
#[cfg(not(feature = "mainnet"))]
const BANNER: &str = "EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS";
#[cfg(feature = "mainnet")]
const BANNER: &str = "EXPERIMENTAL — UNAUDITED research code, built with mainnet support";
/// A seed made when the test runs: nothing that could be a key is kept in
/// the repository.
fn random_seed() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .expect("a source of randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn run(seed: Option<&str>, args: &[&str]) -> (Option<i32>, String, String) {
    let mut command = Command::new(BIN);
    command.args(args).stdin(Stdio::null());
    match seed {
        Some(seed) => command.env("SILENT_WALLET_SEED", seed),
        None => command.env_remove("SILENT_WALLET_SEED"),
    };
    command
        .env_remove("SILENT_WALLET_PHRASE")
        .env_remove("SILENT_WALLET_PASSPHRASE");
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with(BANNER), "{stderr}");
    // (An empty or four-character "seed" would be found in any text.)
    if let Some(seed) = seed.filter(|seed| seed.len() >= 5) {
        assert!(
            !stdout.contains(seed) && !stderr.contains(seed),
            "the seed was printed"
        );
    }
    (output.status.code(), stdout, stderr)
}

#[test]
fn the_address_comes_from_the_seed_in_the_environment() {
    let seed = random_seed();
    let SEED = seed.as_str();
    let (code, regtest, _) = run(Some(SEED), &["address"]);
    assert_eq!(code, Some(0));
    assert!(regtest.starts_with("sprt1"), "{regtest}");
    assert_eq!(
        run(Some(SEED), &["address"]).1,
        regtest,
        "the same every time"
    );
    let (_, signet, _) = run(Some(SEED), &["--network", "signet", "address"]);
    assert!(signet.starts_with("tsp1"), "{signet}");
    let (code, labelled, stderr) = run(Some(SEED), &["address", "--label", "3"]);
    assert_eq!(code, Some(0));
    assert_ne!(labelled, regtest);
    assert!(stderr.contains("only found by a store that scans for label 3"));
    assert_ne!(run(Some(&random_seed()), &["address"]).1, regtest);

    // No seed, or something that is not one: an error that says what is
    // expected and repeats nothing of what was given.
    let (code, stdout, stderr) = run(None, &["address"]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty()
            && stderr.contains("SILENT_WALLET_SEED is not set")
            && stderr.contains("This program does not store either"),
        "{stderr}"
    );
    for bad in ["hello", "0011", ""] {
        let (code, stdout, stderr) = run(Some(bad), &["address"]);
        assert_eq!(code, Some(1), "`{bad}`");
        assert!(
            stdout.is_empty() && stderr.contains("the seed must be 16 to 64 bytes"),
            "{stderr}"
        );
    }
    // The change label is not handed out.
    let (code, stdout, stderr) = run(Some(SEED), &["address", "--label", "0"]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("change label"),
        "{stderr}"
    );
    // Mainnet does not exist in a default build, for any command.
    #[cfg(not(feature = "mainnet"))]
    for args in [
        &["--network", "mainnet", "address"][..],
        &["--network", "bitcoin", "status"][..],
        &["--network", "main", "pending"][..],
    ] {
        let (code, stdout, stderr) = run(Some(SEED), args);
        assert_eq!(code, Some(2), "{args:?}");
        assert!(
            stdout.is_empty() && stderr.contains("mainnet is refused"),
            "{stderr}"
        );
    }
    // There is no command that shows a key or the secret in use. (The one
    // command that prints a secret makes a new one: `new-phrase`.)
    let (_, help, _) = run(Some(SEED), &["--help"]);
    for absent in ["keygen", "export", "dump", "seed show", "private"] {
        assert!(!help.to_lowercase().contains(absent), "`{absent}` in help");
    }
}

#[test]
fn a_store_is_bound_to_the_seed_and_shows_nothing_it_did_not_scan() {
    let seed = random_seed();
    let SEED = seed.as_str();
    let dir = std::env::temp_dir().join(format!("silent-wallet-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("wallet.sqlite3").display().to_string();
    let node = FakeCoreRest::serve(&fixture_gen::generate());
    let url = node.url();

    // Nothing exists yet.
    let (code, _, stderr) = run(Some(SEED), &["status", "--db", &db]);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains("create it with `silent-wallet init`"),
        "{stderr}"
    );

    // The node is a regtest node: a store for another network is not made.
    let (code, stdout, stderr) = run(
        Some(SEED),
        &[
            "--network",
            "signet",
            "init",
            "--db",
            &db,
            "--core-rest-url",
            &url,
        ],
    );
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.to_lowercase().contains("regtest"),
        "{stderr}"
    );
    assert!(!std::path::Path::new(&db).exists());

    let (code, stdout, stderr) = run(
        Some(SEED),
        &["init", "--db", &db, "--core-rest-url", &url, "--label", "2"],
    );
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("birthday: 8")
            && stdout.contains("labels  : [0, 2]")
            && stdout.contains("address : sprt1"),
        "{stdout}"
    );
    assert!(stderr.contains("below height 8 will never be found"));
    // Never overwritten.
    let (code, _, stderr) = run(Some(SEED), &["init", "--db", &db, "--core-rest-url", &url]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("refusing to overwrite"), "{stderr}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&db).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    let (code, stdout, stderr) = run(Some(SEED), &["status", "--db", &db]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stdout.contains("nothing scanned yet"), "{stdout}");
    let (code, stdout, stderr) = run(Some(SEED), &["sync", "--db", &db, "--core-rest-url", &url]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(
        stdout.contains("blocks scanned : 1")
            && stdout.contains("newly detected : 0")
            && stdout.contains("caught up      : yes"),
        "{stdout}"
    );
    let (code, stdout, _) = run(Some(SEED), &["status", "--db", &db]);
    assert_eq!(code, Some(0));
    assert!(
        stdout.contains("as of block      : height 8")
            && stdout.contains("unspent          : 0 sat in 0 output(s)")
            && stdout.contains("detected in full blocks from your node")
            && stdout.contains("not in a block yet are not seen")
            && !stdout.to_lowercase().contains("balance"),
        "{stdout}"
    );
    let (code, stdout, _) = run(Some(SEED), &["pending", "--db", &db]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("no transaction is waiting for a block"));

    // Another seed is another wallet: the store refuses it.
    let other_seed = random_seed();
    let other = other_seed.as_str();
    let before = std::fs::read(&db).unwrap();
    for args in [
        vec!["sync", "--db", &db, "--core-rest-url", &url],
        vec![
            "send",
            "--db",
            &db,
            "--core-rest-url",
            &url,
            "--to",
            "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
            "--amount",
            "1000",
            "--fee-rate",
            "1",
            "--no-broadcast",
        ],
    ] {
        let (code, stdout, stderr) = run(Some(other), &args);
        assert_eq!(code, Some(1), "{args:?}");
        assert!(
            stdout.is_empty() && stderr.contains("is not the one this store was created with"),
            "{stderr}"
        );
    }
    // The store is for regtest.
    let (code, _, stderr) = run(Some(SEED), &["--network", "signet", "status", "--db", &db]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("is for regtest"), "{stderr}");

    // Nothing to spend, and destinations for other networks are not read.
    let send = |to: &str, extra: &[&str]| {
        let mut args = vec![
            "send",
            "--db",
            &db,
            "--core-rest-url",
            &url,
            "--to",
            to,
            "--amount",
            "1000",
            "--fee-rate",
            "1",
        ];
        args.extend_from_slice(extra);
        run(Some(SEED), &args)
    };
    let (code, stdout, stderr) = send(
        "bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080",
        &["--no-broadcast"],
    );
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty()
            && stderr.contains("not enough unspent outputs")
            && stderr.contains("nothing was sent"),
        "{stderr}"
    );
    for foreign in [
        "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        "tb1qw508d6qejxtdg4y5r3zarvary0c5xw7kxpjzsx",
        "sp1qqgste7k9hx0qftg6qmwlkqtwuy6cycyavzmzj85c6qdfhjdpdjtdgqjuexzk6murw56suy3e0rd2cgqvycxttddwsvgxe2usfpxumr70xc9pkqwv",
    ] {
        let (code, stdout, stderr) = send(foreign, &["--no-broadcast"]);
        assert_eq!(code, Some(1), "{foreign}");
        assert!(stdout.is_empty() && stderr.to_lowercase().contains("address"), "{stderr}");
    }
    // Sending needs the cookie file to be named; there is no way around the
    // confirmation.
    let (code, _, stderr) = send("bcrt1qw508d6qejxtdg4y5r3zarvary0c5xw7kygt080", &[]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("--rpc-cookie-file"), "{stderr}");
    let (_, help, _) = run(Some(SEED), &["send", "--help"]);
    assert!(!help.contains("--yes") && !help.contains("--force"));

    assert_eq!(std::fs::read(&db).unwrap(), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A build with the `mainnet` feature: the address carries the mainnet
/// prefix and comes from other keys than the test networks' (coin type 0),
/// and a payment above the default limit is refused before anything is
/// opened, built or signed.
#[cfg(feature = "mainnet")]
#[test]
fn mainnet_build_has_its_own_keys_and_a_default_amount_limit() {
    let seed = random_seed();
    let SEED = seed.as_str();
    let (code, mainnet, _) = run(Some(SEED), &["--network", "mainnet", "address"]);
    assert_eq!(code, Some(0));
    let mainnet = mainnet.trim();
    assert!(mainnet.starts_with("sp1"), "{mainnet}");
    let (_, signet, _) = run(Some(SEED), &["--network", "signet", "address"]);
    let signet = signet.trim();
    assert!(signet.starts_with("tsp1"), "{signet}");
    // Same keys would give the same data part after the prefix.
    assert_ne!(mainnet[3..40], signet[4..41]);

    let dir = std::env::temp_dir().join(format!("silent-wallet-mainnet-{}", std::process::id()));
    let db = dir.join("absent.sqlite3");
    let db = db.to_str().unwrap();
    let send = |extra: &[&str]| {
        let mut args = vec![
            "--network",
            "mainnet",
            "send",
            "--db",
            db,
            "--core-rest-url",
            "http://127.0.0.1:1",
            "--to",
            mainnet,
            "--fee-rate",
            "1",
            "--no-broadcast",
        ];
        args.extend_from_slice(extra);
        run(Some(SEED), &args)
    };
    let (code, stdout, stderr) = send(&["--amount", "100001"]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("more than the limit of 100000 sat"),
        "{stderr}"
    );
    // At the limit, and above it once the limit is raised, the refusal is
    // gone: the next thing missing is the store.
    for extra in [
        &["--amount", "100000"][..],
        &["--amount", "100001", "--max-amount", "100001"][..],
    ] {
        let (code, _, stderr) = send(extra);
        assert_eq!(code, Some(1));
        assert!(
            !stderr.contains("more than the limit") && stderr.contains("cannot open the store"),
            "{stderr}"
        );
    }
}

/// `--max-amount` is a limit on every network; only its default differs.
#[test]
fn a_payment_above_the_given_limit_is_refused_before_anything_is_opened() {
    let seed = random_seed();
    let SEED = seed.as_str();
    let (_, address, _) = run(Some(SEED), &["address"]);
    let (code, stdout, stderr) = run(
        Some(SEED),
        &[
            "send",
            "--db",
            "/nonexistent/absent.sqlite3",
            "--core-rest-url",
            "http://127.0.0.1:1",
            "--to",
            address.trim(),
            "--fee-rate",
            "1",
            "--no-broadcast",
            "--amount",
            "11",
            "--max-amount",
            "10",
        ],
    );
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("more than the limit of 10 sat"),
        "{stderr}"
    );
    // Without a limit, on a test network, the amount is not what stops it.
    let (_, _, stderr) = run(
        Some(SEED),
        &[
            "send",
            "--db",
            "/nonexistent/absent.sqlite3",
            "--core-rest-url",
            "http://127.0.0.1:1",
            "--to",
            address.trim(),
            "--fee-rate",
            "1",
            "--no-broadcast",
            "--amount",
            "100001",
        ],
    );
    assert!(
        !stderr.contains("more than the limit") && stderr.contains("cannot open the store"),
        "{stderr}"
    );
}

/// A seed phrase instead of the hexadecimal seed: `new-phrase` makes one
/// (shown once, on stdout only), and the wallet it gives is the one the
/// library derives from the same words. No other command prints it.
#[test]
fn a_seed_phrase_can_be_made_and_used_instead_of_the_seed() {
    let phrase_run =
        |phrase: Option<&str>, passphrase: Option<&str>, seed: Option<&str>, args: &[&str]| {
            let mut command = Command::new(BIN);
            command.args(args).stdin(Stdio::null());
            for (name, value) in [
                ("SILENT_WALLET_PHRASE", phrase),
                ("SILENT_WALLET_PASSPHRASE", passphrase),
                ("SILENT_WALLET_SEED", seed),
            ] {
                match value {
                    Some(value) => command.env(name, value),
                    None => command.env_remove(name),
                };
            }
            let output = command.output().unwrap();
            (
                output.status.code(),
                String::from_utf8(output.stdout).unwrap(),
                String::from_utf8(output.stderr).unwrap(),
            )
        };
    let mut phrases = Vec::new();
    for (words, n) in [(None, 12), (Some("12"), 12), (Some("24"), 24)] {
        let mut args = vec!["new-phrase"];
        if let Some(words) = words {
            args.extend(["--words", words]);
        }
        let (code, stdout, stderr) = phrase_run(None, None, None, &args);
        assert_eq!(code, Some(0), "{stderr}");
        let phrase = stdout.trim().to_string();
        assert_eq!(phrase.split(' ').count(), n);
        assert!(stderr.contains("Write them down") && !stderr.contains(&phrase));
        phrases.push(phrase);
    }
    assert_ne!(phrases[0], phrases[1]);
    for bad in ["15", "13", "0"] {
        let (code, stdout, _) = phrase_run(None, None, None, &["new-phrase", "--words", bad]);
        assert_ne!(code, Some(0));
        assert!(stdout.is_empty());
    }

    let phrase = phrases[0].as_str();
    let (code, address, stderr) = phrase_run(Some(phrase), None, None, &["address"]);
    assert_eq!(code, Some(0), "{stderr}");
    assert!(!stderr.contains(phrase) && !address.contains(phrase));
    let expected = silent_wallet::client::SeedPhrase::parse(phrase)
        .unwrap()
        .keys("", silent_sync::Network::Regtest)
        .unwrap()
        .receiver()
        .address(silent_sync::Network::Regtest)
        .to_string();
    assert_eq!(address.trim(), expected);
    // The passphrase is part of the wallet.
    let (_, other, _) = phrase_run(Some(phrase), Some("more"), None, &["address"]);
    assert!(other.starts_with("sprt1") && other != address);

    // Not a phrase; or both secrets at once: refused, nothing repeated.
    let wrong = phrase.replacen(' ', " zebra ", 1);
    let (code, stdout, stderr) = phrase_run(Some(&wrong), None, None, &["address"]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty()
            && stderr.contains("not a valid seed phrase")
            && !stderr.contains("zebra"),
        "{stderr}"
    );
    let seed = random_seed();
    let (code, stdout, stderr) = phrase_run(Some(phrase), None, Some(&seed), &["address"]);
    assert_eq!(code, Some(1));
    assert!(
        stdout.is_empty() && stderr.contains("set exactly one of them"),
        "{stderr}"
    );
}
