//! `silent-sync` — thin experimental CLI over `sync-core`.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! This binary only parses arguments, calls `sync-core` and prints. It has no
//! send, sign or balance functionality.

mod lab_keys;

use clap::{Parser, Subcommand, ValueEnum};
use std::{path::PathBuf, process::ExitCode};
use sync_core::{
    engine::{scan_range, ScanReport},
    network::{ChainIdentity, LabNetwork},
    protocol::ReceiveScanner,
    source::{fixture::FixtureSource, ScanSource},
    EXPERIMENTAL_BANNER,
};

/// Exit code for "understood, but not implemented in this phase".
const EXIT_NOT_IMPLEMENTED: u8 = 3;

#[derive(Parser)]
#[command(
    name = "silent-sync",
    version,
    about = "Silent Sync Lab — experimental BIP352 receive-side sync lab",
    long_about = "Silent Sync Lab — experimental BIP352 (Silent Payments) receive-side sync lab.\n\n\
                  EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS. Unaudited research code.\n\
                  Uses a fixed, public, test-only key. Detection only: no sending, no balances."
)]
struct Cli {
    /// Network to operate on: `regtest` or `signet`. Mainnet is refused.
    #[arg(long, global = true, default_value = "regtest", value_parser = parse_network)]
    network: LabNetwork,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report which backends are usable in this build.
    Doctor,
    /// Show the lab's test-only silent payment address.
    Address {
        #[command(subcommand)]
        action: AddressAction,
    },
    /// Scan a block range for outputs paid to the lab receiver.
    ///
    /// Stateless in this phase: nothing is persisted and there is no resume
    /// (planned for Phase 3).
    Sync(SyncArgs),
}

#[derive(Subcommand)]
enum AddressAction {
    /// Print the address (public data only).
    Show {
        /// Print the labelled address for label integer `m` instead.
        #[arg(long)]
        label: Option<u32>,
    },
}

#[derive(clap::Args)]
struct SyncArgs {
    /// Data source.
    #[arg(long, value_enum)]
    backend: Backend,
    /// Fixture file (required for `--backend fixture`).
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// First height to scan [default: 1].
    #[arg(long)]
    from: Option<u32>,
    /// Last height to scan [default: the source's tip].
    #[arg(long)]
    to: Option<u32>,
    /// Label integer to scan for in addition to the change label (repeatable).
    #[arg(long = "label")]
    labels: Vec<u32>,
    /// Print the report as JSON on stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Backend {
    /// Deterministic offline fixture file.
    Fixture,
    /// Bitcoin Core (not implemented yet — Phase 2).
    Core,
    /// BlindBit Oracle v2 (not implemented yet — Phase 4).
    #[value(name = "oracle-v2")]
    OracleV2,
}

fn parse_network(value: &str) -> Result<LabNetwork, String> {
    value.parse::<LabNetwork>().map_err(|e| e.to_string())
}

struct Failure {
    code: u8,
    message: String,
}

impl Failure {
    fn error(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }

    fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            code: EXIT_NOT_IMPLEMENTED,
            message: message.into(),
        }
    }
}

fn main() -> ExitCode {
    // Shown before anything else, including argument errors (design spec §5.10).
    eprintln!("{EXPERIMENTAL_BANNER}");
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("error: {}", failure.message);
            ExitCode::from(failure.code)
        }
    }
}

fn run(cli: Cli) -> Result<(), Failure> {
    match cli.command {
        Command::Doctor => {
            doctor(cli.network);
            Ok(())
        }
        Command::Address {
            action: AddressAction::Show { label },
        } => address_show(cli.network, label),
        Command::Sync(args) => sync(cli.network, args),
    }
}

fn doctor(network: LabNetwork) {
    println!("silent-sync {}", env!("CARGO_PKG_VERSION"));
    println!("network           : {network} (mainnet is refused by design)");
    println!("receiver          : {}", lab_keys::DESCRIPTION);
    println!("backend fixture   : available (offline, structurally validated only)");
    println!("backend core      : NOT IMPLEMENTED (Phase 2)");
    println!("backend oracle-v2 : NOT IMPLEMENTED (Phase 4)");
    println!("persistence       : NOT IMPLEMENTED (Phase 3) — `sync` is a stateless scan");
}

fn address_show(network: LabNetwork, label: Option<u32>) -> Result<(), Failure> {
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let code = match label {
        None => keys.address(network),
        Some(m) => keys
            .labelled_address(network, m)
            .map_err(|e| Failure::error(e.to_string()))?,
    };
    println!("{code}");
    eprintln!("receiver: {}", lab_keys::DESCRIPTION);
    if network == LabNetwork::Regtest {
        eprintln!(
            "note: the `sprt` prefix is an upstream bdk-sp convention for regtest; BIP352 \
             itself only defines `sp` (mainnet) and `tsp` (test networks)."
        );
    }
    match label {
        Some(0) => eprintln!("note: label 0 is reserved for change and must not be handed out."),
        Some(m) => eprintln!(
            "note: label {m} must be backed up. A wallet restored without it cannot find \
             payments to this address, nor later outputs in the same transaction."
        ),
        None => {}
    }
    Ok(())
}

fn sync(network: LabNetwork, args: SyncArgs) -> Result<(), Failure> {
    let source = match args.backend {
        Backend::Fixture => {
            let path = args
                .fixture
                .as_deref()
                .ok_or_else(|| Failure::error("`--backend fixture` requires `--fixture <file>`"))?;
            FixtureSource::from_path(path).map_err(|e| Failure::error(e.to_string()))?
        }
        Backend::Core => {
            return Err(Failure::not_implemented(
                "backend `core` is not implemented yet (Phase 2). Nothing was scanned.",
            ))
        }
        Backend::OracleV2 => {
            return Err(Failure::not_implemented(
                "backend `oracle-v2` is not implemented yet (Phase 4). Nothing was scanned.",
            ))
        }
    };

    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let scanner =
        ReceiveScanner::new(&keys, &args.labels).map_err(|e| Failure::error(e.to_string()))?;
    let tip = source.tip().map_err(|e| Failure::error(e.to_string()))?;
    let from = args.from.unwrap_or(1);
    let to = args.to.unwrap_or(tip.height);

    eprintln!(
        "scanning {from}..={to} on `{}` ({network}); stateless, nothing is persisted",
        source.backend_id()
    );
    let report = scan_range(
        &source,
        &scanner,
        ChainIdentity::for_network(network),
        from,
        to,
    )
    .map_err(|e| {
        Failure::error(format!(
            "{e}\nscan aborted: no block of the range is reported as scanned"
        ))
    })?;

    if args.json {
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| Failure::error(format!("cannot serialize report: {e}")))?;
        println!("{json}");
    } else {
        print_report(&report);
    }
    Ok(())
}

fn print_report(report: &ScanReport) {
    println!("backend              : {}", report.backend_id);
    println!("network              : {}", report.chain.network);
    println!(
        "range                : {} ({}) ..= {} ({})",
        report.first_block.height,
        report.first_block.hash,
        report.last_block.height,
        report.last_block.hash
    );
    println!("labels scanned       : {:?}", report.labels_scanned);
    println!("blocks scanned       : {}", report.blocks_scanned);
    println!("transactions seen    : {}", report.txs_seen);
    println!("transactions scanned : {}", report.txs_scanned);
    for (reason, count) in &report.txs_skipped {
        println!("  not scanned ({reason}): {count}");
    }
    println!(
        "detected_received_outputs: {}",
        report.detected_received_outputs.len()
    );
    for d in &report.detected_received_outputs {
        let label = d.label.map_or_else(|| "-".to_string(), |m| m.to_string());
        println!(
            "  height {:>4}  {}:{}  {} sat  label {}",
            d.block.height, d.txid, d.vout, d.value_sats, label
        );
    }
    println!("note: detected outputs are not a balance; spent status is not tracked.");
}
