//! Records which compiler and target built the binary, for benchmark results.

use std::process::Command;

fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |v| v.trim().to_string());
    println!("cargo:rustc-env=LAB_RUSTC_VERSION={version}");
    println!(
        "cargo:rustc-env=LAB_TARGET={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
    );
    println!("cargo:rerun-if-changed=build.rs");
}
