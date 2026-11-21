//! Regenerates `fixtures/local/regtest_basic_v1.json`, or writes a long
//! deterministic chain for scale tests and benchmarks.
//!
//! ```sh
//! cargo run -p lab-testkit --bin gen_fixture -- fixtures/local/regtest_basic_v1.json
//! cargo run -p lab-testkit --bin gen_fixture -- --long 1000 /path/outside/repo/long_1000.json
//! ```
//!
//! Test-data generation only; see `lab_testkit::fixture_gen`.

use lab_testkit::fixture_gen::{self, LongChain};

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let (path, json) = match args.as_slice() {
        [path] => (path, fixture_gen::generate_json()),
        [flag, blocks, path] if flag == "--long" => {
            let Ok(blocks @ 1..=100_000) = blocks.parse::<u32>() else {
                eprintln!("--long takes a block count from 1 to 100000");
                return ExitCode::from(2);
            };
            let fixture = fixture_gen::generate_long(&LongChain::standard(blocks));
            match serde_json::to_string(&fixture) {
                Ok(json) => (path, json),
                Err(e) => {
                    eprintln!("cannot encode the fixture: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        _ => {
            eprintln!("usage: gen_fixture [--long <blocks>] <output.json>");
            return ExitCode::from(2);
        }
    };
    match fs::write(path, json) {
        Ok(()) => {
            eprintln!("wrote {path}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("cannot write {path}: {e}");
            ExitCode::FAILURE
        }
    }
}
