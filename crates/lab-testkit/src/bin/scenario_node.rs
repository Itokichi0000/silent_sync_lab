//! Starts a throwaway regtest node, puts the standard silent payment scenario
//! on it and keeps it running until stdin is closed (or a line is entered).
//!
//! ```sh
//! BITCOIND_EXE=/path/to/bitcoind cargo run -p lab-testkit --bin scenario_node
//! ```
//!
//! Prints one JSON object on stdout: the REST URL, heights and the outputs the
//! sender created for the lab receiver. For manual experiments, e.g. pointing
//! an index server at the node. TEST-ONLY: regtest, loopback, temporary data.

use lab_testkit::{
    regtest_node::Node,
    regtest_wallet::{standard_scenario, Faucet},
};
use std::io::{BufRead, Write};

fn main() {
    let extra_blocks: u32 = std::env::args()
        .nth(1)
        .map(|a| {
            a.parse()
                .expect("usage: scenario_node [extra-empty-blocks]")
        })
        .unwrap_or(0);
    let node = Node::regtest();
    let mut faucet = Faucet::new(&node);
    let scenario = standard_scenario(&mut faucet);
    if extra_blocks > 0 {
        node.generate_to(extra_blocks, faucet.miner_address());
    }
    let info = serde_json::json!({
        "rest_url": node.rest_url(),
        "base_height": scenario.base_height,
        "scenario_tip_height": scenario.tip_height,
        "tip_height": node.block_count(),
        "expected_detections": scenario.expected,
    });
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{info}").expect("stdout");
    stdout.flush().expect("stdout");
    eprintln!("node is running; close stdin or press Enter to stop it");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
}
