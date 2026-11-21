//! The wallet's command line without a real node: keys from the environment,
//! refusals, and an empty wallet against an in-process fake of Core's REST
//! interface. Sending is covered against a real node in `regtest_wallet.rs`.
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report
#![allow(non_snake_case)] // `SEED` is a per-test value used like a constant

use lab_testkit::{fake_http::FakeCoreRest, fixture_gen};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_silent-wallet");
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
    let output = command.output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.starts_with("EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS"));
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
            && stderr.contains("does not create, store or show seeds"),
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
    // Mainnet does not exist here, for any command.
    for args in [
        &["--network", "mainnet", "address"][..],
        &["--network", "bitcoin", "status"][..],
        &["--network", "main", "pending"][..],
    ] {
        let (code, stdout, stderr) = run(Some(SEED), args);
        assert_eq!(code, Some(2), "{args:?}");
        assert!(
            stdout.is_empty() && stderr.to_lowercase().contains("mainnet"),
            "{stderr}"
        );
    }
    // There is no command that makes or shows a key.
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
