//! `silent-sync` — thin experimental CLI over `sync-core`.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! This binary only parses arguments, calls `sync-core` / `lab-storage` and
//! prints. It has no send, sign or balance functionality.

mod lab_keys;

use clap::{Parser, Subcommand, ValueEnum};
use lab_storage::SqliteStore;
use std::{
    fmt::Write as _,
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};
use sync_core::{
    engine::{scan_range, ScanReport},
    models::DetectedOutput,
    network::{ChainIdentity, LabNetwork},
    protocol::ReceiveScanner,
    source::{
        bitcoin_core::CoreRestSource,
        blindbit::BlindbitHttpSource,
        fixture::FixtureSource,
        local_index::{IndexPolicy, LocalTweakIndex},
        ScanSource,
    },
    store::{StoreMeta, SyncStore},
    sync::{new_store_meta, sync, SyncOptions, SyncReport},
    verify::{cross_check, VerificationReport},
    EXPERIMENTAL_BANNER,
};

const DEFAULT_DB: &str = "silent-sync-lab.sqlite3";
/// Exit code of `verify` when the two backends do not agree.
const EXIT_DIFFERENCES: u8 = 4;
const DEFAULT_ORACLE_URL: &str = "http://127.0.0.1:8000";

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
    /// Report which backends are usable; probes the services whose URLs are given.
    Doctor {
        /// REST URL of a local Bitcoin Core node to probe, e.g. http://127.0.0.1:18443.
        #[arg(long)]
        core_rest_url: Option<String>,
        /// HTTP URL of a local BlindBit Oracle v2 to probe (needs --core-rest-url
        /// too: the node supplies the headers the oracle is checked against).
        #[arg(long)]
        oracle_url: Option<String>,
    },
    /// Show the lab's test-only silent payment address.
    Address {
        #[command(subcommand)]
        action: AddressAction,
    },
    /// Manage the local lab store.
    Lab {
        #[command(subcommand)]
        action: LabAction,
    },
    /// Stateless scan of a block range: nothing is stored, nothing is resumed.
    Scan(ScanArgs),
    /// Bring the lab store up to the source's tip: resumes from the stored
    /// checkpoint and handles reorganizations.
    Sync(SyncArgs),
    /// Show what the lab store is bound to and how far it is synced.
    Status {
        #[command(flatten)]
        db: DbArg,
        /// Print as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Scan the same range through two backends and explain every difference.
    Verify(VerifyArgs),
    /// Inspect what was detected.
    Received {
        #[command(subcommand)]
        action: ReceivedAction,
    },
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

#[derive(Subcommand)]
enum LabAction {
    /// Create the lab store and bind it to a network, backend, label set and
    /// wallet birthday. An existing store is never overwritten.
    Init {
        #[command(flatten)]
        db: DbArg,
        #[command(flatten)]
        source: SourceArgs,
        /// First block height to scan. Payments confirmed below it will never
        /// be found, so choose a height at or below the wallet's first use.
        #[arg(long)]
        birthday: u32,
        /// Why that height is safe (recorded in the store).
        #[arg(long)]
        birthday_basis: String,
        /// Label integer to scan for in addition to the change label
        /// (repeatable). Cannot be changed later without a new store.
        #[arg(long = "label")]
        labels: Vec<u32>,
    },
}

#[derive(Subcommand)]
enum ReceivedAction {
    /// List detected outputs. This is not a balance: spent status is not tracked.
    List {
        #[command(flatten)]
        db: DbArg,
        /// Print as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
}

#[derive(clap::Args)]
struct DbArg {
    /// Path of the lab store.
    #[arg(long, default_value = DEFAULT_DB)]
    db: PathBuf,
}

#[derive(clap::Args)]
struct SourceArgs {
    /// Data source.
    #[arg(long, value_enum)]
    backend: Backend,
    /// Fixture file (required for `--backend fixture`).
    #[arg(long)]
    fixture: Option<PathBuf>,
    /// REST URL of your own Bitcoin Core node (>= 30, started with -rest=1) for
    /// `--backend core`. Loopback only
    /// [default: http://127.0.0.1:18443 on regtest, http://127.0.0.1:38332 on signet].
    #[arg(long)]
    core_rest_url: Option<String>,
    /// HTTP URL of a BlindBit Oracle v2 on this machine, for `--backend
    /// oracle-v2`. Block identity is still taken from your own node
    /// (`--core-rest-url`); the oracle only supplies the tweak index.
    #[arg(long)]
    oracle_url: Option<String>,
    /// For `--backend local-index`: which transactions the simulated index lists.
    #[arg(long, value_enum, default_value = "bip352")]
    index_policy: IndexPolicyArg,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum IndexPolicyArg {
    /// Exactly the transactions BIP352 says to scan.
    Bip352,
    /// Also transactions spending SegWit v2+ outputs (as BlindBit Oracle v2
    /// was observed to list).
    IgnoreSegwitVersionRule,
}

#[derive(clap::Args)]
struct VerifyArgs {
    /// The backend being checked.
    #[command(flatten)]
    candidate: SourceArgs,
    /// The full-block backend to check against.
    #[arg(long, value_enum)]
    reference: ReferenceBackend,
    /// Fixture file for `--reference fixture` [default: the candidate's --fixture].
    #[arg(long)]
    reference_fixture: Option<PathBuf>,
    /// REST URL for `--reference core` [default: the candidate's --core-rest-url].
    #[arg(long)]
    reference_core_rest_url: Option<String>,
    /// First height [default: 1].
    #[arg(long)]
    from: Option<u32>,
    /// Last height [default: the reference's tip].
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
enum ReferenceBackend {
    Fixture,
    Core,
}

#[derive(clap::Args)]
struct ScanArgs {
    #[command(flatten)]
    source: SourceArgs,
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

#[derive(clap::Args)]
struct SyncArgs {
    #[command(flatten)]
    db: DbArg,
    #[command(flatten)]
    source: SourceArgs,
    /// Stop at this height instead of the source's tip.
    #[arg(long)]
    to: Option<u32>,
    /// Stop after this many blocks (whole chunks); the store stays resumable.
    #[arg(long)]
    max_blocks: Option<u64>,
    /// Blocks fetched, scanned and committed per step.
    #[arg(long, default_value_t = 100)]
    chunk_size: u32,
    /// Pause between chunks (milliseconds). For demonstrations and tests of
    /// interruption; has no other effect.
    #[arg(long, default_value_t = 0, hide = true)]
    chunk_delay_ms: u64,
    /// Print the report as JSON on stdout.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Backend {
    /// Deterministic offline fixture file.
    Fixture,
    /// Your own Bitcoin Core node, via its REST interface.
    Core,
    /// BlindBit Oracle v2 tweak index over HTTP, anchored by your own node's headers.
    #[value(name = "oracle-v2")]
    OracleV2,
    /// A tweak index computed locally from full blocks (the fixture if
    /// --fixture is given, otherwise your node). A model of an index server
    /// for comparisons; nothing is fetched from a third party.
    LocalIndex,
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
}

fn fail(e: impl std::fmt::Display) -> Failure {
    Failure::error(e.to_string())
}

/// Writes `text` to stdout in one go. A closed or broken stdout is reported
/// as an error instead of panicking (as `println!` would): the consumer did
/// not receive the result, so the command must not look successful.
fn emit(text: &str) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Failure::error(format!("cannot write to stdout: {e}")))
}

fn emit_json(value: &impl serde::Serialize) -> Result<(), Failure> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| Failure::error(format!("cannot serialize: {e}")))?;
    emit(&(json + "\n"))
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
    let network = cli.network;
    match cli.command {
        Command::Doctor {
            core_rest_url,
            oracle_url,
        } => doctor(network, core_rest_url.as_deref(), oracle_url.as_deref()),
        Command::Verify(args) => verify(network, args),
        Command::Address {
            action: AddressAction::Show { label },
        } => address_show(network, label),
        Command::Lab {
            action:
                LabAction::Init {
                    db,
                    source,
                    birthday,
                    birthday_basis,
                    labels,
                },
        } => lab_init(network, &db.db, &source, birthday, &birthday_basis, &labels),
        Command::Scan(args) => scan(network, args),
        Command::Sync(args) => sync_command(network, args),
        Command::Status { db, json } => status(network, &db.db, json),
        Command::Received {
            action: ReceivedAction::List { db, json },
        } => received_list(network, &db.db, json),
    }
}

fn default_core_rest_url(network: LabNetwork) -> &'static str {
    match network {
        LabNetwork::Regtest => "http://127.0.0.1:18443",
        LabNetwork::Signet => "http://127.0.0.1:38332",
    }
}

fn open_source(network: LabNetwork, args: &SourceArgs) -> Result<Box<dyn ScanSource>, Failure> {
    let source: Box<dyn ScanSource> = match args.backend {
        Backend::Fixture => {
            let path = args
                .fixture
                .as_deref()
                .ok_or_else(|| Failure::error("`--backend fixture` requires `--fixture <file>`"))?;
            Box::new(FixtureSource::from_path(path).map_err(fail)?)
        }
        Backend::Core => {
            let url = args
                .core_rest_url
                .as_deref()
                .unwrap_or_else(|| default_core_rest_url(network));
            eprintln!("connecting to Bitcoin Core REST (loopback)");
            Box::new(
                CoreRestSource::connect(url)
                    .map_err(|e| Failure::error(format!("{e}\nnothing was scanned")))?,
            )
        }
        Backend::OracleV2 => {
            let core_url = args
                .core_rest_url
                .as_deref()
                .unwrap_or_else(|| default_core_rest_url(network));
            let oracle_url = args.oracle_url.as_deref().unwrap_or(DEFAULT_ORACLE_URL);
            eprintln!("connecting to BlindBit Oracle (loopback); headers from Bitcoin Core REST");
            let headers = CoreRestSource::connect(core_url)
                .map_err(|e| Failure::error(format!("header source: {e}\nnothing was scanned")))?;
            Box::new(
                BlindbitHttpSource::connect(oracle_url, headers)
                    .map_err(|e| Failure::error(format!("{e}\nnothing was scanned")))?,
            )
        }
        Backend::LocalIndex => {
            let policy = match args.index_policy {
                IndexPolicyArg::Bip352 => IndexPolicy::Bip352,
                IndexPolicyArg::IgnoreSegwitVersionRule => IndexPolicy::IgnoreSegwitVersionRule,
            };
            match args.fixture.as_deref() {
                Some(path) => Box::new(LocalTweakIndex::new(
                    FixtureSource::from_path(path).map_err(fail)?,
                    policy,
                )),
                None => {
                    let url = args
                        .core_rest_url
                        .as_deref()
                        .unwrap_or_else(|| default_core_rest_url(network));
                    Box::new(LocalTweakIndex::new(
                        CoreRestSource::connect(url)
                            .map_err(|e| Failure::error(format!("{e}\nnothing was scanned")))?,
                        policy,
                    ))
                }
            }
        }
    };
    let expected = ChainIdentity::for_network(network);
    if source.chain() != expected {
        return Err(Failure::error(format!(
            "chain mismatch: --network is {network} but source `{}` serves {}\nnothing was scanned",
            source.backend_id(),
            source.chain().network()
        )));
    }
    Ok(source)
}

fn doctor(
    network: LabNetwork,
    core_rest_url: Option<&str>,
    oracle_url: Option<&str>,
) -> Result<(), Failure> {
    // `Err` = a backend the user asked about cannot be used.
    let core: Result<String, String> = match core_rest_url {
        None => Ok(format!(
            "not probed (needs your own Bitcoin Core >= 30 with -rest=1; pass \
             --core-rest-url, e.g. {})",
            default_core_rest_url(network)
        )),
        Some(url) => match CoreRestSource::connect(url).and_then(|s| {
            let tip = s.tip()?;
            Ok((s.chain(), tip, s.is_pruned()))
        }) {
            Ok((chain, tip, pruned)) if chain.network() == network => Ok(format!(
                "reachable: {} at height {}{}",
                chain.network(),
                tip.height,
                if pruned {
                    " (PRUNED: blocks below the prune height cannot be scanned)"
                } else {
                    ""
                }
            )),
            Ok((chain, _, _)) => Err(format!(
                "UNUSABLE: the node is on {}, but --network is {network}",
                chain.network()
            )),
            Err(e) => Err(format!("UNAVAILABLE: {e}")),
        },
    };
    let oracle: Result<String, String> = match oracle_url {
        None => Ok("not probed (pass --oracle-url and --core-rest-url)".to_string()),
        Some(url) => {
            let core_url = core_rest_url.unwrap_or_else(|| default_core_rest_url(network));
            CoreRestSource::connect(core_url)
                .and_then(|headers| BlindbitHttpSource::connect(url, headers))
                .and_then(|s| {
                    let tip = s.tip()?;
                    // Does the oracle have, and agree on, the node's tip block?
                    s.fetch_range(tip.height, tip.height)?;
                    Ok(tip)
                })
                .map(|tip| {
                    format!(
                        "reachable: indexes the node's chain up to its tip (height {})",
                        tip.height
                    )
                })
                .map_err(|e| format!("UNAVAILABLE: {e}"))
        }
    };
    let usable = core.is_ok() && oracle.is_ok();
    let core = core.unwrap_or_else(|e| e);
    let oracle = oracle.unwrap_or_else(|e| e);
    let lines = [
        format!("silent-sync {}", env!("CARGO_PKG_VERSION")),
        format!("network           : {network} (mainnet is refused by design)"),
        format!("receiver          : {}", lab_keys::DESCRIPTION),
        "backend fixture   : available (offline, structurally validated only)".to_string(),
        format!("backend core      : {core}"),
        format!("backend oracle-v2 : {oracle}"),
        "backend local-index: available (tweak index computed locally from full blocks)"
            .to_string(),
        "persistence       : SQLite store (`lab init`, `sync`, `status`, `received list`)"
            .to_string(),
    ];
    emit(&(lines.join("\n") + "\n"))?;
    if usable {
        Ok(())
    } else {
        Err(Failure::error("a probed backend is not usable (see above)"))
    }
}

fn address_show(network: LabNetwork, label: Option<u32>) -> Result<(), Failure> {
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let code = match label {
        None => keys.address(network),
        Some(m) => keys.labelled_address(network, m).map_err(fail)?,
    };
    emit(&format!("{code}\n"))?;
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

fn scan(network: LabNetwork, args: ScanArgs) -> Result<(), Failure> {
    let source = open_source(network, &args.source)?;
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let scanner = ReceiveScanner::new(&keys, &args.labels).map_err(fail)?;
    let tip = source.tip().map_err(fail)?;
    let from = args.from.unwrap_or(1);
    let to = args.to.unwrap_or(tip.height);

    eprintln!(
        "scanning {from}..={to} on `{}` ({network}); stateless, nothing is persisted",
        source.backend_id()
    );
    let report = scan_range(
        source.as_ref(),
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
        emit_json(&report)
    } else {
        emit(&render_scan_report(&report))
    }
}

fn verify(network: LabNetwork, args: VerifyArgs) -> Result<(), Failure> {
    let candidate = open_source(network, &args.candidate)?;
    let reference_args = SourceArgs {
        backend: match args.reference {
            ReferenceBackend::Fixture => Backend::Fixture,
            ReferenceBackend::Core => Backend::Core,
        },
        fixture: args
            .reference_fixture
            .or_else(|| args.candidate.fixture.clone()),
        core_rest_url: args
            .reference_core_rest_url
            .or_else(|| args.candidate.core_rest_url.clone()),
        oracle_url: None,
        index_policy: IndexPolicyArg::Bip352,
    };
    let reference = open_source(network, &reference_args)?;
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let scanner = ReceiveScanner::new(&keys, &args.labels).map_err(fail)?;
    let from = args.from.unwrap_or(1);
    let to = match args.to {
        Some(to) => to,
        None => reference.tip().map_err(fail)?.height,
    };
    eprintln!(
        "cross-checking `{}` against `{}` over {from}..={to} ({network})",
        candidate.backend_id(),
        reference.backend_id()
    );
    let report = cross_check(
        candidate.as_ref(),
        reference.as_ref(),
        &scanner,
        ChainIdentity::for_network(network),
        from,
        to,
    )
    .map_err(|e| Failure::error(format!("{e}\nverification aborted: no result")))?;

    if args.json {
        emit_json(&report)?;
    } else {
        emit(&render_verification(&report))?;
    }
    if report.identical {
        Ok(())
    } else {
        Err(Failure {
            code: EXIT_DIFFERENCES,
            message: format!(
                "the backends differ: {} only in the candidate, {} only in the reference, {} unexplained",
                report.candidate_only, report.reference_only, report.unexplained
            ),
        })
    }
}

fn render_verification(report: &VerificationReport) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    line(format!(
        "candidate            : {}",
        report.candidate_backend
    ));
    line(format!(
        "reference            : {}",
        report.reference_backend
    ));
    line(format!("network              : {}", report.chain.network()));
    line(format!(
        "range                : {}..={}",
        report.first_height, report.last_height
    ));
    line(format!(
        "labels scanned       : {:?}",
        report.labels_scanned
    ));
    line(format!("agreed outputs       : {}", report.agreed));
    line(format!("only in candidate    : {}", report.candidate_only));
    line(format!("only in reference    : {}", report.reference_only));
    line(format!("unexplained          : {}", report.unexplained));
    if !report.block_hash_mismatches.is_empty() {
        line(format!(
            "block hash mismatches: heights {:?}",
            report.block_hash_mismatches
        ));
    }
    for d in &report.differences {
        let explanation = serde_json::to_string(&d.explanation).unwrap_or_default();
        line(format!(
            "  height {:>4}  {}:{}  {}",
            d.height, d.txid, d.vout, explanation
        ));
    }
    line(format!(
        "verdict              : {}",
        if report.identical {
            "identical"
        } else {
            "DIFFERENT"
        }
    ));
    line(
        "note: agreement shows the candidate served what the reference shows for this receiver; \
         it does not prove either is complete."
            .to_string(),
    );
    out
}

fn lab_init(
    network: LabNetwork,
    db: &Path,
    source_args: &SourceArgs,
    birthday: u32,
    birthday_basis: &str,
    labels: &[u32],
) -> Result<(), Failure> {
    if birthday_basis.trim().is_empty() {
        return Err(Failure::error(
            "`--birthday-basis` must say why the birthday height is safe",
        ));
    }
    let source = open_source(network, source_args)?;
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    let scanner = ReceiveScanner::new(&keys, labels).map_err(fail)?;
    let meta = new_store_meta(
        source.as_ref(),
        &scanner,
        &keys.wallet_id(),
        birthday,
        birthday_basis.trim(),
    );
    let mut store = SqliteStore::create(db).map_err(fail)?;
    if let Err(e) = store.initialize(&meta) {
        drop(store);
        // Do not leave a half-made store behind.
        let _ = std::fs::remove_file(db);
        return Err(fail(e));
    }
    eprintln!("created {}", db.display());
    eprintln!(
        "note: payments confirmed below height {birthday} will never be found by this store."
    );
    if meta.labels.iter().any(|&m| m != 0) {
        eprintln!(
            "note: the label set {:?} is fixed for this store; back the labels up.",
            meta.labels
        );
    }
    emit(&render_meta(db, &meta, None, 0, 0))
}

/// Opens the store and checks it against `--network`.
fn open_store(network: LabNetwork, db: &Path) -> Result<(SqliteStore, StoreMeta), Failure> {
    let store = SqliteStore::open(db).map_err(fail)?;
    let meta = store.meta().map_err(fail)?.ok_or_else(|| {
        Failure::error(format!(
            "{} is not initialized; run `lab init`",
            db.display()
        ))
    })?;
    if meta.chain.network() != network {
        return Err(Failure::error(format!(
            "store {} is for {}, but --network is {network}",
            db.display(),
            meta.chain.network()
        )));
    }
    Ok((store, meta))
}

fn sync_command(network: LabNetwork, args: SyncArgs) -> Result<(), Failure> {
    let (mut store, meta) = open_store(network, &args.db.db)?;
    let source = open_source(network, &args.source)?;
    let keys = lab_keys::lab_receiver_keys().map_err(Failure::error)?;
    // The label set is part of the store's binding, not a per-run choice.
    let extra: Vec<u32> = meta.labels.iter().copied().filter(|&m| m != 0).collect();
    let scanner = ReceiveScanner::new(&keys, &extra).map_err(fail)?;
    let wallet_id = keys.wallet_id();
    let aborted = |e: sync_core::sync::SyncError| {
        Failure::error(format!(
            "{e}\nsync aborted: the store keeps what was committed before the error and can be resumed"
        ))
    };

    eprintln!(
        "syncing {} on `{}` ({network}) from the stored checkpoint",
        args.db.db.display(),
        source.backend_id()
    );
    let mut options = SyncOptions {
        chunk_size: args.chunk_size,
        target_height: args.to,
        max_blocks: args.max_blocks,
    };

    // One `sync` call per chunk when a delay is requested, so that the pause
    // sits between commits. The reports are merged.
    let report = if args.chunk_delay_ms == 0 {
        sync(source.as_ref(), &scanner, &wallet_id, &mut store, &options).map_err(aborted)?
    } else {
        let mut merged: Option<SyncReport> = None;
        let mut remaining = args.max_blocks;
        loop {
            options.max_blocks =
                Some(u64::from(args.chunk_size).min(remaining.unwrap_or(u64::MAX)));
            let step = sync(source.as_ref(), &scanner, &wallet_id, &mut store, &options)
                .map_err(aborted)?;
            if let Some(cp) = step.checkpoint {
                eprintln!("committed up to height {}", cp.height);
            }
            let done = step.caught_up || step.blocks_scanned == 0;
            if let Some(left) = remaining.as_mut() {
                *left = left.saturating_sub(step.blocks_scanned);
            }
            let stop = done || remaining == Some(0);
            merged = Some(match merged {
                None => step,
                Some(mut all) => {
                    all.blocks_scanned += step.blocks_scanned;
                    all.txs_seen += step.txs_seen;
                    all.txs_scanned += step.txs_scanned;
                    for (reason, count) in step.txs_skipped {
                        *all.txs_skipped.entry(reason).or_default() += count;
                    }
                    all.new_detections.extend(step.new_detections);
                    all.checkpoint = step.checkpoint;
                    all.caught_up = step.caught_up;
                    all
                }
            });
            if stop {
                break;
            }
            std::thread::sleep(Duration::from_millis(args.chunk_delay_ms));
        }
        merged.ok_or_else(|| Failure::error("sync made no step"))?
    };

    if args.json {
        emit_json(&report)
    } else {
        emit(&render_sync_report(&report))
    }
}

#[derive(serde::Serialize)]
struct StatusJson<'a> {
    store: String,
    binding: &'a StoreMeta,
    checkpoint: Option<sync_core::models::BlockRef>,
    blocks_stored: u64,
    detected_received_outputs: u64,
}

fn status(network: LabNetwork, db: &Path, json: bool) -> Result<(), Failure> {
    let (store, meta) = open_store(network, db)?;
    let checkpoint = store.checkpoint().map_err(fail)?;
    let blocks = store.block_count().map_err(fail)?;
    let detected = store.detections().map_err(fail)?.len() as u64;
    if json {
        emit_json(&StatusJson {
            store: db.display().to_string(),
            binding: &meta,
            checkpoint,
            blocks_stored: blocks,
            detected_received_outputs: detected,
        })
    } else {
        emit(&render_meta(db, &meta, checkpoint, blocks, detected))
    }
}

fn received_list(network: LabNetwork, db: &Path, json: bool) -> Result<(), Failure> {
    let (store, _) = open_store(network, db)?;
    let detections = store.detections().map_err(fail)?;
    if json {
        emit_json(&detections)
    } else {
        let mut out = String::new();
        let _ = writeln!(out, "detected_received_outputs: {}", detections.len());
        render_detections(&mut out, &detections);
        let _ = writeln!(
            out,
            "note: detected outputs are not a balance; spent status is not tracked."
        );
        emit(&out)
    }
}

fn render_detections(out: &mut String, detections: &[DetectedOutput]) {
    for d in detections {
        let label = d.label.map_or_else(|| "-".to_string(), |m| m.to_string());
        let _ = writeln!(
            out,
            "  height {:>4}  {}:{}  {} sat  label {}",
            d.block.height, d.txid, d.vout, d.value_sats, label
        );
    }
}

fn render_meta(
    db: &Path,
    meta: &StoreMeta,
    checkpoint: Option<sync_core::models::BlockRef>,
    blocks: u64,
    detected: u64,
) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    line(format!("store                : {}", db.display()));
    line(format!("network              : {}", meta.chain.network()));
    line(format!("backend              : {}", meta.backend_id));
    line(format!("wallet id (public)   : {}", meta.wallet_id));
    line(format!("labels scanned       : {:?}", meta.labels));
    line(format!("birthday             : {}", meta.birthday));
    line(format!("birthday basis       : {}", meta.birthday_basis));
    line(format!("scanner version      : {}", meta.scanner_version));
    match checkpoint {
        Some(cp) => line(format!(
            "checkpoint           : {} ({})",
            cp.height, cp.hash
        )),
        None => line("checkpoint           : none (nothing scanned yet)".to_string()),
    }
    line(format!("blocks stored        : {blocks}"));
    line(format!("detected_received_outputs: {detected}"));
    line("note: detected outputs are not a balance; spent status is not tracked.".to_string());
    out
}

fn render_scan_report(report: &ScanReport) -> String {
    let mut out = String::new();
    {
        let mut line = |text: String| {
            let _ = writeln!(out, "{text}");
        };
        line(format!("backend              : {}", report.backend_id));
        line(format!("network              : {}", report.chain.network()));
        line(format!(
            "range                : {} ({}) ..= {} ({})",
            report.first_block.height,
            report.first_block.hash,
            report.last_block.height,
            report.last_block.hash
        ));
        line(format!(
            "labels scanned       : {:?}",
            report.labels_scanned
        ));
        line(format!("blocks scanned       : {}", report.blocks_scanned));
        line(format!("transactions seen    : {}", report.txs_seen));
        line(format!("transactions scanned : {}", report.txs_scanned));
        for (reason, count) in &report.txs_skipped {
            line(format!("  not scanned ({reason}): {count}"));
        }
        line(format!(
            "detected_received_outputs: {}",
            report.detected_received_outputs.len()
        ));
    }
    render_detections(&mut out, &report.detected_received_outputs);
    let _ = writeln!(
        out,
        "note: detected outputs are not a balance; spent status is not tracked."
    );
    out
}

fn render_sync_report(report: &SyncReport) -> String {
    let mut out = String::new();
    {
        let mut line = |text: String| {
            let _ = writeln!(out, "{text}");
        };
        line(format!("backend              : {}", report.backend_id));
        line(format!("network              : {}", report.chain.network()));
        match report.resumed_from {
            Some(cp) => line(format!(
                "resumed from         : {} ({})",
                cp.height, cp.hash
            )),
            None => line(format!(
                "resumed from         : nothing stored; starting at birthday {}",
                report.birthday
            )),
        }
        if let Some(reorg) = &report.reorg {
            line(format!(
                "REORG                : checkpoint {} ({}) is no longer on the chain",
                reorg.abandoned_checkpoint.height, reorg.abandoned_checkpoint.hash
            ));
            line(format!(
                "  rolled back to height {} ({} blocks, {} detections removed)",
                reorg.fork_height,
                reorg.blocks_rolled_back,
                reorg.detections_removed.len()
            ));
        }
        line(format!("blocks scanned       : {}", report.blocks_scanned));
        line(format!("transactions seen    : {}", report.txs_seen));
        line(format!("transactions scanned : {}", report.txs_scanned));
        match report.checkpoint {
            Some(cp) => line(format!(
                "checkpoint           : {} ({})",
                cp.height, cp.hash
            )),
            None => line("checkpoint           : none".to_string()),
        }
        line(format!(
            "source tip           : {} ({})",
            report.source_tip.height, report.source_tip.hash
        ));
        line(format!(
            "caught up            : {}",
            if report.caught_up {
                "yes"
            } else {
                "no (stopped by --to / --max-blocks; run sync again to continue)"
            }
        ));
        line(format!(
            "newly detected_received_outputs: {}",
            report.new_detections.len()
        ));
    }
    render_detections(&mut out, &report.new_detections);
    let _ = writeln!(
        out,
        "note: detected outputs are not a balance; spent status is not tracked."
    );
    out
}
