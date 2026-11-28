//! The wallet end to end on a real regtest node: receive, see what is
//! unspent, send to another wallet, and see both sides after the block.
//! Ignored by default; needs `BITCOIND_EXE` (see `scripts/fetch_bitcoind.sh`).
#![allow(clippy::unwrap_used)] // test code: a panic is the failure report

use lab_testkit::{
    regtest_node::Node,
    regtest_wallet::{signed_transaction, silent_payment_scripts, Faucet, InputKind},
};
use silent_sync::{Network, SilentPaymentAddress};
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

const BIN: &str = env!("CARGO_BIN_EXE_silent-wallet");

/// A throwaway seed, made when the test runs: nothing secret is kept in the
/// repository.
fn random_seed() -> String {
    use std::io::Read as _;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .expect("a source of randomness");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs the wallet with `seed` in its environment and `input` on stdin.
fn wallet(seed: &str, args: &[&str], input: &str) -> Run {
    let mut child = Command::new(BIN)
        .args(args)
        .env("SILENT_WALLET_SEED", seed)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    let run = Run {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    };
    // The seed never comes back out.
    for text in [&run.stdout, &run.stderr] {
        assert!(!text.to_lowercase().contains(seed), "the seed was printed");
    }
    assert!(run.stderr.starts_with("EXPERIMENTAL — "));
    run
}

fn ok(seed: &str, args: &[&str]) -> String {
    let run = wallet(seed, args, "");
    assert_eq!(run.code, Some(0), "{args:?}: {}", run.stderr);
    run.stdout
}

fn json(seed: &str, args: &[&str]) -> serde_json::Value {
    serde_json::from_str(&ok(seed, args)).unwrap()
}

#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn receive_then_send_between_two_wallets() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let url = node.rest_url();
    let cookie = node.cookie_file().display().to_string();
    let (alice, bob) = (random_seed(), random_seed());
    let dir = std::env::temp_dir().join(format!("silent-wallet-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_a = dir.join("alice.sqlite3").display().to_string();
    let db_b = dir.join("bob.sqlite3").display().to_string();

    // Each wallet's address, and a store bound to its keys and this node.
    let address_a = ok(&alice, &["address"]).trim().to_string();
    let address_b = ok(&bob, &["address"]).trim().to_string();
    assert!(address_a.starts_with("sprt1") && address_b.starts_with("sprt1"));
    assert_ne!(address_a, address_b);
    for (seed, db) in [(&alice, &db_a), (&bob, &db_b)] {
        ok(seed, &["init", "--db", db, "--core-rest-url", &url]);
    }
    // The other seed does not open a store.
    let wrong = wallet(&bob, &["sync", "--db", &db_a, "--core-rest-url", &url], "");
    assert_eq!(wrong.code, Some(1));
    assert!(
        wrong
            .stderr
            .contains("is not the one this store was created with"),
        "{}",
        wrong.stderr
    );

    // Two payments to Alice, in two transactions and two blocks.
    let to_alice = SilentPaymentAddress::parse(&address_a, Network::Regtest).unwrap();
    let mut received = Vec::new();
    for funded in faucet.fund(&[InputKind::P2wpkh, InputKind::P2trKeyPath]) {
        let amount = funded.prevout.value.to_sat() - 1_000;
        let scripts = silent_payment_scripts(std::slice::from_ref(&funded), &[to_alice]);
        let tx = signed_transaction(
            std::slice::from_ref(&funded),
            vec![(amount, scripts[0].clone())],
        );
        faucet.broadcast_and_mine(&tx);
        received.push((tx.compute_txid().to_string(), amount));
    }
    let total: u64 = received.iter().map(|(_, amount)| amount).sum();

    let synced = json(
        &alice,
        &["sync", "--db", &db_a, "--core-rest-url", &url, "--json"],
    );
    assert_eq!(synced["caught_up"], true);
    assert_eq!(synced["new_detections"].as_array().unwrap().len(), 2);
    let status = json(&alice, &["status", "--db", &db_a, "--json"]);
    let unspent = status["unspent"].as_array().unwrap();
    assert_eq!(unspent.len(), 2);
    for (held, (txid, amount)) in unspent.iter().zip(&received) {
        assert_eq!(held["output"]["txid"], *txid);
        assert_eq!(held["output"]["value_sats"], *amount);
        assert_eq!(
            held["output"]["provenance"]["evidence_kind"],
            "full_block_with_prevouts"
        );
    }
    let text = ok(&alice, &["status", "--db", &db_a]);
    assert!(
        text.contains(&format!("unspent          : {total} sat in 2 output(s)"))
            && text.contains("detected in full blocks from your node")
            && !text.to_lowercase().contains("balance"),
        "{text}"
    );

    // An output can be held back: listed apart, not counted, not spent.
    let apart = format!("{}:{}", received[0].0, unspent[0]["output"]["vout"]);
    assert!(ok(&alice, &["hold", "--db", &db_a, "--outpoint", &apart]).contains("is held back"));
    let status = json(&alice, &["status", "--db", &db_a, "--json"]);
    assert_eq!(status["unspent"].as_array().unwrap().len(), 1);
    assert_eq!(status["held_back"].as_array().unwrap().len(), 1);
    let text = ok(&alice, &["status", "--db", &db_a]);
    assert!(
        text.contains(&format!(
            "unspent          : {} sat in 1 output(s)",
            received[1].1
        )) && text.contains("held back by you"),
        "{text}"
    );
    let unknown = format!("{}:7", received[0].0);
    let refused = wallet(&alice, &["hold", "--db", &db_a, "--outpoint", &unknown], "");
    assert_eq!(refused.code, Some(1));
    assert!(refused.stderr.contains("cannot be held back"));
    assert!(ok(
        &alice,
        &["hold", "--db", &db_a, "--outpoint", &apart, "--release"]
    )
    .contains("is no longer held back"));
    let status = json(&alice, &["status", "--db", &db_a, "--json"]);
    assert_eq!(status["unspent"].as_array().unwrap().len(), 2);

    // Alice pays Bob more than one of her outputs holds: two inputs, change.
    let amount = received[0].1 + 10_000;
    let send = |input: &str, extra: &[&str]| {
        let amount = amount.to_string();
        let mut args = vec![
            "send",
            "--db",
            &db_a,
            "--core-rest-url",
            &url,
            "--to",
            &address_b,
            "--amount",
            &amount,
            "--fee-rate",
            "2",
        ];
        args.extend_from_slice(extra);
        wallet(&alice, &args, input)
    };
    let with_cookie = ["--rpc-cookie-file", cookie.as_str(), "--json"];
    // Not confirmed: nothing is sent.
    for answer in ["", "yes\n", "send\n"] {
        let refused = send(answer, &with_cookie);
        assert_eq!(refused.code, Some(1), "{answer:?}");
        assert!(
            refused.stdout.is_empty() && refused.stderr.contains("not confirmed; nothing was sent"),
            "{}",
            refused.stderr
        );
    }
    assert_eq!(
        node.cli_json(&["getrawmempool"]).as_array().unwrap().len(),
        0
    );
    // Without a way to authenticate, nothing is even built for sending.
    let no_cookie = send("SEND\n", &["--rpc-cookie-file", "/nonexistent/.cookie"]);
    assert_eq!(no_cookie.code, Some(1));
    assert!(no_cookie.stderr.contains("cannot read the cookie file"));

    // Signed and printed only.
    let dry = send("", &["--no-broadcast", "--json"]);
    assert_eq!(dry.code, Some(0), "{}", dry.stderr);
    let printed: serde_json::Value = serde_json::from_str(&dry.stdout).unwrap();
    assert_eq!(printed["broadcast"], false);
    assert_eq!(
        node.cli_json(&["getrawmempool"]).as_array().unwrap().len(),
        0
    );
    // The node itself judges the printed transaction valid.
    let verdict = node.cli_json(&[
        "testmempoolaccept",
        &format!("[\"{}\"]", printed["raw_transaction_hex"].as_str().unwrap()),
    ]);
    assert_eq!(verdict[0]["allowed"], true, "{verdict}");

    // Held back while the confirmation is awaited: the transaction shown
    // spends it, so nothing is sent (both outputs are inputs).
    {
        use std::io::Read as _;
        let amount = amount.to_string();
        let mut child = Command::new(BIN)
            .args([
                "send",
                "--db",
                &db_a,
                "--core-rest-url",
                &url,
                "--to",
                &address_b,
                "--amount",
                &amount,
                "--fee-rate",
                "2",
            ])
            .args(with_cookie)
            .env("SILENT_WALLET_SEED", &alice)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let mut shown = Vec::new();
        let mut byte = [0u8; 1];
        while !String::from_utf8_lossy(&shown).contains("to send it: ") {
            assert_eq!(stderr.read(&mut byte).unwrap(), 1, "the prompt never came");
            shown.push(byte[0]);
        }
        assert!(ok(&alice, &["hold", "--db", &db_a, "--outpoint", &apart]).contains("is held back"));
        child.stdin.take().unwrap().write_all(b"SEND\n").unwrap();
        let mut rest = String::new();
        stderr.read_to_string(&mut rest).unwrap();
        let status = child.wait().unwrap();
        assert_eq!(status.code(), Some(1), "{rest}");
        assert!(
            rest.contains("was held back or spent by another transaction")
                && rest.contains("nothing was sent"),
            "{rest}"
        );
        assert_eq!(
            node.cli_json(&["getrawmempool"]).as_array().unwrap().len(),
            0
        );
        assert!(ok(&alice, &["pending", "--db", &db_a])
            .contains("no transaction is waiting for a block"));
        assert!(ok(
            &alice,
            &["hold", "--db", &db_a, "--outpoint", &apart, "--release"]
        )
        .contains("is no longer held back"));
    }

    // Confirmed: the node takes it.
    let sent = send("SEND\n", &with_cookie);
    assert_eq!(sent.code, Some(0), "{}", sent.stderr);
    assert!(
        sent.stderr.contains("it cannot be taken back") && sent.stderr.contains("Type SEND"),
        "{}",
        sent.stderr
    );
    let result: serde_json::Value = serde_json::from_str(&sent.stdout).unwrap();
    assert_eq!(result["broadcast"], true);
    assert_eq!(result["inputs"].as_array().unwrap().len(), 2);
    assert_eq!(result["payments"][0]["amount_sats"], amount);
    assert_eq!(result["payments"][0]["to"], address_b);
    let change = result["change"]["amount_sats"].as_u64().unwrap();
    let fee = result["fee_sats"].as_u64().unwrap();
    assert_eq!(amount + change + fee, total);
    let txid = result["txid"].as_str().unwrap().to_string();
    let mempool = node.cli_json(&["getrawmempool"]);
    assert_eq!(mempool, serde_json::json!([txid]));
    // The credentials are nowhere in the output.
    let secret = std::fs::read_to_string(node.cookie_file()).unwrap();
    let password = secret.trim().split(':').nth(1).unwrap();
    assert!(!sent.stdout.contains(password) && !sent.stderr.contains(password));

    // Until a block contains it, its inputs are neither unspent nor spent,
    // and cannot be offered to a second transaction.
    let waiting = json(&alice, &["status", "--db", &db_a, "--json"]);
    assert_eq!(waiting["unspent"].as_array().unwrap().len(), 0);
    assert_eq!(waiting["spent_by_pending"].as_array().unwrap().len(), 2);
    assert_eq!(waiting["spent"], 0);
    assert!(ok(&alice, &["pending", "--db", &db_a]).contains(&txid));
    // Nor held back: it is being spent already.
    let held = wallet(&alice, &["hold", "--db", &db_a, "--outpoint", &apart], "");
    assert_eq!(held.code, Some(1));
    assert!(
        held.stderr.contains("cannot be held back"),
        "{}",
        held.stderr
    );
    let again = send("SEND\n", &with_cookie);
    assert_eq!(again.code, Some(1));
    assert!(
        again.stderr.contains("not enough unspent outputs")
            && again.stderr.contains("nothing was sent"),
        "{}",
        again.stderr
    );

    // The block arrives.
    node.generate_to(1, faucet.miner_address());
    let after = wallet(
        &alice,
        &["sync", "--db", &db_a, "--core-rest-url", &url],
        "",
    );
    assert_eq!(after.code, Some(0), "{}", after.stderr);
    assert!(
        after.stdout.contains("newly spent    : 2")
            && after.stdout.contains("newly detected : 1")
            && after
                .stdout
                .contains(&format!("sent transaction {txid} is in a block")),
        "{}",
        after.stdout
    );
    let settled = json(&alice, &["status", "--db", &db_a, "--json"]);
    assert_eq!(settled["spent"], 2);
    assert_eq!(settled["spent_by_pending"].as_array().unwrap().len(), 0);
    let unspent = settled["unspent"].as_array().unwrap();
    assert_eq!(unspent.len(), 1, "only the change is left");
    assert_eq!(unspent[0]["output"]["txid"], txid);
    assert_eq!(unspent[0]["output"]["value_sats"], change);
    assert_eq!(
        unspent[0]["output"]["label"], 0,
        "found under the change label"
    );
    assert!(ok(&alice, &["pending", "--db", &db_a]).contains("no transaction is waiting"));

    // Bob finds what Alice sent.
    let bobs = json(
        &bob,
        &["sync", "--db", &db_b, "--core-rest-url", &url, "--json"],
    );
    let found = bobs["new_detections"].as_array().unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0]["txid"], txid);
    assert_eq!(found[0]["value_sats"], amount);
    assert_eq!(found[0]["vout"], result["payments"][0]["vout"]);
    let bob_status = ok(&bob, &["status", "--db", &db_b]);
    assert!(
        bob_status.contains(&format!("unspent          : {amount} sat in 1 output(s)")),
        "{bob_status}"
    );

    // And Bob can pay it on: to an ordinary address, everything but the fee.
    let ordinary = node.cli(&["getnewaddress", "", "bech32m"]);
    let onward = (amount - 5_000).to_string();
    let paid = wallet(
        &bob,
        &[
            "send",
            "--db",
            &db_b,
            "--core-rest-url",
            &url,
            "--to",
            &ordinary,
            "--amount",
            &onward,
            "--fee-rate",
            "1",
            "--rpc-cookie-file",
            &cookie,
        ],
        "SEND\n",
    );
    assert_eq!(paid.code, Some(0), "{}", paid.stderr);
    assert!(
        paid.stdout.contains("status      : sent to your node"),
        "{}",
        paid.stdout
    );
    node.generate_to(1, faucet.miner_address());
    let received_by_node = node.cli(&["getreceivedbyaddress", &ordinary, "1"]);
    assert!(
        received_by_node.parse::<f64>().unwrap() > 0.0,
        "{received_by_node}"
    );

    // Mainnet cannot be asked for.
    let mainnet = wallet(&alice, &["--network", "mainnet", "address"], "");
    assert_ne!(mainnet.code, Some(0));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A wallet with a fresh seed and store on `node`, and `outputs` payments of
/// 99,000 sat to it, each in its own block, scanned.
fn funded_wallet(node: &Node, faucet: &mut Faucet<'_>, db: &str, outputs: usize) -> String {
    let seed = random_seed();
    let url = node.rest_url();
    let address = ok(&seed, &["address"]).trim().to_string();
    ok(&seed, &["init", "--db", db, "--core-rest-url", &url]);
    let to = SilentPaymentAddress::parse(&address, Network::Regtest).unwrap();
    for funded in faucet.fund(&vec![InputKind::P2wpkh; outputs]) {
        let amount = funded.prevout.value.to_sat() - 1_000;
        let scripts = silent_payment_scripts(std::slice::from_ref(&funded), &[to]);
        let tx = signed_transaction(
            std::slice::from_ref(&funded),
            vec![(amount, scripts[0].clone())],
        );
        faucet.broadcast_and_mine(&tx);
    }
    ok(&seed, &["sync", "--db", db, "--core-rest-url", &url]);
    seed
}

/// Forwards every request to the node. For `sendrawtransaction` the node's
/// answer is thrown away and the connection closed: the node has the
/// transaction, the caller never learns it.
fn proxy_that_loses_the_answer_to_a_send(node_port: u16) -> u16 {
    use std::{
        io::Read as _,
        net::{TcpListener, TcpStream},
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(mut client) = client else { return };
            let mut request = Vec::new();
            let mut buffer = [0u8; 65536];
            loop {
                let n = client.read(&mut buffer).unwrap_or(0);
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
            let mut upstream = TcpStream::connect(("127.0.0.1", node_port)).unwrap();
            upstream.write_all(&request).unwrap();
            let mut response = Vec::new();
            upstream.read_to_end(&mut response).unwrap();
            if !String::from_utf8_lossy(&request).contains("sendrawtransaction") {
                let _ = client.write_all(&response);
            }
        }
    });
    port
}

/// Found by review: when the answer to the send is lost, the node has the
/// transaction while the wallet reported a failure, and a repeated command
/// paid a second time. Now the outcome is called unknown, the transaction is
/// on record before it leaves, and its inputs are not offered again.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn a_lost_answer_is_not_reported_as_a_failed_send() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let url = node.rest_url();
    let cookie = node.cookie_file().display().to_string();
    let dir = std::env::temp_dir().join(format!("silent-wallet-lost-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("alice.sqlite3").display().to_string();
    // Two outputs: enough to pay twice, if the wallet let it happen.
    let alice = funded_wallet(&node, &mut faucet, &db, 2);
    let recipient = node.cli(&["getnewaddress", "", "bech32m"]);
    let node_port: u16 = url.rsplit(':').next().unwrap().parse().unwrap();
    let lossy = format!(
        "http://127.0.0.1:{}",
        proxy_that_loses_the_answer_to_a_send(node_port)
    );
    let send = |rpc_url: &str| {
        wallet(
            &alice,
            &[
                "send",
                "--db",
                &db,
                "--core-rest-url",
                &url,
                "--to",
                &recipient,
                "--amount",
                "20000",
                "--fee-rate",
                "1",
                "--rpc-url",
                rpc_url,
                "--rpc-cookie-file",
                &cookie,
            ],
            "SEND\n",
        )
    };

    let lost = send(&lossy);
    assert_eq!(lost.code, Some(1));
    assert!(
        lost.stdout.is_empty()
            && lost
                .stderr
                .contains("IT IS NOT KNOWN whether your node took transaction")
            && lost.stderr.contains("Do NOT repeat the payment")
            && !lost.stderr.contains("nothing was sent"),
        "{}",
        lost.stderr
    );
    // The node does have it ...
    let mempool = node.cli_json(&["getrawmempool"]);
    let in_mempool = mempool.as_array().unwrap();
    assert_eq!(in_mempool.len(), 1);
    let txid = in_mempool[0].as_str().unwrap().to_string();
    assert!(lost.stderr.contains(&txid));
    // ... and the wallet holds its input back.
    assert!(ok(&alice, &["pending", "--db", &db]).contains(&txid));
    let status = json(&alice, &["status", "--db", &db, "--json"]);
    assert_eq!(status["spent_by_pending"].as_array().unwrap().len(), 1);
    assert_eq!(status["unspent"].as_array().unwrap().len(), 1);

    // After the block, the wallet knows.
    node.generate_to(1, faucet.miner_address());
    let synced = ok(&alice, &["sync", "--db", &db, "--core-rest-url", &url]);
    assert!(
        synced.contains(&format!("sent transaction {txid} is in a block")),
        "{synced}"
    );
    // The recipient was paid once.
    let received = node.cli(&["getreceivedbyaddress", &recipient, "1"]);
    assert_eq!(received.parse::<f64>().unwrap(), 0.0002, "{received}");

    // A node that says no is a definite failure: nothing stays on record.
    // (The same payment again spends the change and the other output; ask
    // for more than there is left to be refused by the wallet instead, and
    // use a dead port for "never asked".)
    let dead = send("http://127.0.0.1:9");
    assert_eq!(dead.code, Some(1));
    assert!(
        dead.stderr.contains("the node is unavailable") && dead.stderr.contains("nothing was"),
        "{}",
        dead.stderr
    );
    assert!(ok(&alice, &["pending", "--db", &db]).contains("no transaction is waiting"));
    assert_eq!(
        node.cli_json(&["getrawmempool"]).as_array().unwrap().len(),
        0
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Found by review: after the block with a sent transaction was abandoned,
/// its input looked unspent again while the transaction sat in the mempool,
/// and the next payment replaced it. Now the transaction goes back on hold.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn a_reorganization_puts_a_sent_transaction_back_on_hold() {
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let url = node.rest_url();
    let cookie = node.cookie_file().display().to_string();
    let dir = std::env::temp_dir().join(format!("silent-wallet-reorg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("alice.sqlite3").display().to_string();
    let alice = funded_wallet(&node, &mut faucet, &db, 1);
    let send = |to: &str, amount: &str, rate: &str| {
        wallet(
            &alice,
            &[
                "send",
                "--db",
                &db,
                "--core-rest-url",
                &url,
                "--to",
                to,
                "--amount",
                amount,
                "--fee-rate",
                rate,
                "--rpc-cookie-file",
                &cookie,
                "--json",
            ],
            "SEND\n",
        )
    };
    let first_recipient = node.cli(&["getnewaddress", "", "bech32m"]);
    let first = send(&first_recipient, "20000", "1");
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let txid = serde_json::from_str::<serde_json::Value>(&first.stdout).unwrap()["txid"]
        .as_str()
        .unwrap()
        .to_string();
    let hashes = node.generate_to(1, faucet.miner_address());
    let confirmed = ok(&alice, &["sync", "--db", &db, "--core-rest-url", &url]);
    assert!(confirmed.contains(&format!("sent transaction {txid} is in a block")));
    // Still on record: its block is not deep.
    assert!(ok(&alice, &["pending", "--db", &db]).contains(&format!("{txid}  1 input(s)")));

    // The block is abandoned for a competing one without the payment.
    node.cli(&["invalidateblock", &hashes[0]]);
    node.generate_block_with(faucet.miner_address(), &[]);
    assert_eq!(node.cli_json(&["getrawmempool"]), serde_json::json!([txid]));
    let reorged = ok(&alice, &["sync", "--db", &db, "--core-rest-url", &url]);
    assert!(
        reorged.contains("REORG")
            && reorged.contains("1 spends reverted")
            && reorged.contains(&format!(
                "sent transaction {txid} is NOT in a block any more"
            )),
        "{reorged}"
    );
    let status = json(&alice, &["status", "--db", &db, "--json"]);
    assert_eq!(status["unspent"].as_array().unwrap().len(), 0);
    assert_eq!(status["spent_by_pending"].as_array().unwrap().len(), 1);

    // Another payment cannot take the same input: the first one stands.
    let other = node.cli(&["getnewaddress", "", "bech32m"]);
    let second = send(&other, "30000", "3");
    assert_eq!(second.code, Some(1));
    assert!(
        second.stderr.contains("not enough unspent outputs"),
        "{}",
        second.stderr
    );
    assert_eq!(node.cli_json(&["getrawmempool"]), serde_json::json!([txid]));

    // It confirms again.
    node.generate_to(1, faucet.miner_address());
    let again = ok(&alice, &["sync", "--db", &db, "--core-rest-url", &url]);
    assert!(
        again.contains(&format!("sent transaction {txid} is in a block")),
        "{again}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Found by review: only the REST endpoint's chain was checked. A node on
/// another chain at the RPC address — mainnet here — is refused before a
/// transaction is built, let alone shown for confirmation.
#[test]
#[ignore = "needs BITCOIND_EXE (Bitcoin Core >= 30)"]
fn a_transaction_is_never_handed_to_a_node_on_another_chain() {
    use lab_testkit::regtest_node::NodeChain;
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let mainnet = Node::start(NodeChain::OfflineMainnet);
    let url = node.rest_url();
    let dir = std::env::temp_dir().join(format!("silent-wallet-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("alice.sqlite3").display().to_string();
    let alice = funded_wallet(&node, &mut faucet, &db, 1);
    let recipient = node.cli(&["getnewaddress", "", "bech32m"]);
    let send = |rpc_url: &str, cookie: &str| {
        wallet(
            &alice,
            &[
                "send",
                "--db",
                &db,
                "--core-rest-url",
                &url,
                "--to",
                &recipient,
                "--amount",
                "20000",
                "--fee-rate",
                "1",
                "--rpc-url",
                rpc_url,
                "--rpc-cookie-file",
                cookie,
            ],
            "SEND\n",
        )
    };
    let refused = send(
        &mainnet.rest_url(),
        &mainnet.cookie_file().display().to_string(),
    );
    assert_eq!(refused.code, Some(1));
    assert!(
        refused.stdout.is_empty()
            && refused
                .stderr
                .contains("on another chain than the wallet's store")
            && refused.stderr.contains("nothing was built or sent")
            && !refused.stderr.contains("Type SEND"),
        "{}",
        refused.stderr
    );
    // Credentials typed into the URL are refused without being repeated.
    let with_credentials = send(
        "http://rpcuser:hunter2secret@127.0.0.1:18443",
        &node.cookie_file().display().to_string(),
    );
    assert_eq!(with_credentials.code, Some(1));
    assert!(
        with_credentials.stderr.contains("refusing the RPC URL")
            && !with_credentials.stderr.contains("hunter2secret"),
        "{}",
        with_credentials.stderr
    );
    assert!(ok(&alice, &["pending", "--db", &db]).contains("no transaction is waiting"));
    let _ = std::fs::remove_dir_all(&dir);
}
