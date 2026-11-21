//! Starts a throwaway regtest node with a long chain of signed transactions
//! (and, if `BLINDBIT_ORACLE_EXE` is set, a BlindBit Oracle indexing it) and
//! keeps them running until stdin is closed, for benchmarks.
//!
//! ```sh
//! BITCOIND_EXE=/path/to/bitcoind BLINDBIT_ORACLE_EXE=/path/to/blindbit-oracle \
//!     cargo run --release -p lab-testkit --bin bench_node -- 1000 10
//! ```
//!
//! Arguments: number of blocks, and how often a block pays the lab receiver
//! (default 10). Prints one JSON object on stdout: the URLs, the measured
//! range and how many outputs the sender created for the lab receiver.
//! TEST-ONLY: regtest, loopback, temporary data.

use lab_testkit::{
    oracle_node::{oracle_exe, Oracle},
    regtest_node::Node,
    regtest_wallet::{long_scenario, Faucet},
};
use std::{
    io::{BufRead, Write},
    time::{Duration, Instant},
};

fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: bench_node <blocks> [payment-every]";
    let blocks: u32 = args.next().and_then(|a| a.parse().ok()).expect(usage);
    let payment_every: u32 = args.next().map_or(10, |a| a.parse().expect(usage));

    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let started = Instant::now();
    let scenario = long_scenario(&mut faucet, blocks, payment_every);
    eprintln!(
        "chain built in {:.1} s: heights {}..={}",
        started.elapsed().as_secs_f64(),
        scenario.base_height + 1,
        scenario.tip_height
    );

    let oracle = oracle_exe().map(|_| {
        let started = Instant::now();
        let oracle = Oracle::start(&node);
        oracle.wait_for_height_within(scenario.tip_height, Duration::from_secs(1_800));
        eprintln!(
            "oracle indexed up to {} in {:.1} s (includes its start-up)",
            scenario.tip_height,
            started.elapsed().as_secs_f64()
        );
        (oracle, started.elapsed())
    });

    let network_info = node.cli_json(&["getnetworkinfo"]);
    let info = serde_json::json!({
        "rest_url": node.rest_url(),
        "oracle_url": oracle.as_ref().map(|(o, _)| o.url()),
        "from": scenario.base_height + 1,
        "to": scenario.tip_height,
        "blocks": blocks,
        "payment_every": payment_every,
        "expected_detections": scenario.expected.len(),
        "node_version": network_info["subversion"],
        "oracle_indexing_seconds": oracle.as_ref().map(|(_, t)| t.as_secs_f64()),
    });
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{info}").expect("stdout");
    stdout.flush().expect("stdout");
    eprintln!("services are running; close stdin or press Enter to stop them");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
