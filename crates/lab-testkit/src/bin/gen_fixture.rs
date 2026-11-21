//! Regenerates `fixtures/local/regtest_basic_v1.json`.
//!
//! ```sh
//! cargo run -p lab-testkit --bin gen_fixture -- fixtures/local/regtest_basic_v1.json
//! ```
//!
//! Test-data generation only; see `lab_testkit::fixture_gen`.

use lab_testkit::fixture_gen;

use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!("usage: gen_fixture <output.json>");
        return ExitCode::from(2);
    };
    match fs::write(&path, fixture_gen::generate_json()) {
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
