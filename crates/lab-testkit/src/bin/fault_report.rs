//! Runs the fault injection matrix and prints its Markdown table (the body of
//! `docs/FAULT_INJECTION.md`).
//!
//! ```sh
//! cargo run -p lab-testkit --bin fault_report
//! ```

fn main() {
    print!(
        "{}",
        lab_testkit::fault_matrix::render_markdown(&lab_testkit::fault_matrix::run())
    );
}
