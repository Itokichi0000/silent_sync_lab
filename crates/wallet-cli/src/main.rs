//! `silent-wallet` — an experimental command line wallet for silent payments.
//!
//! EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS.
//!
//! A thin frontend: scanning and storage are `silent-sync`'s, building and
//! signing are `silent-wallet`'s. This file reads arguments, asks before
//! anything is sent, and prints.
//!
//! The seed is taken from the environment variable `SILENT_WALLET_SEED`
//! (16 to 64 bytes as hexadecimal digits) on every run. It is never written
//! anywhere and never printed; this program cannot create or show one.

use clap::{Parser, Subcommand};
use silent_sync::{
    bitcoin::{consensus::encode::serialize_hex, Txid},
    source::CoreRestSource,
    sync::{new_store_meta, sync_with_progress, SyncError, SyncOptions, SyncReport},
    DetectedOutput, EvidenceKind, Network, ScanSource, StoreMeta, SyncStore,
};
use silent_sync_sqlite::SqliteStore;
use silent_wallet::{
    build_and_sign,
    client::{SeedPhrase, WordCount, MAINNET_DEFAULT_MAX_AMOUNT_SATS},
    holdings, BuiltTransaction, CoreRpc, Destination, Holdings, Payment, PendingSpends,
    PendingTransaction, SendLimits, Settlement, Wallet, WalletKeys,
};
use std::{
    fmt::Write as _,
    io::{self, BufRead as _, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
};

/// Printed first on every run. A build that can be pointed at mainnet does
/// not claim otherwise.
#[cfg(not(feature = "mainnet"))]
const EXPERIMENTAL_BANNER: &str =
    "EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS (unaudited research code)";
#[cfg(feature = "mainnet")]
const EXPERIMENTAL_BANNER: &str =
    "EXPERIMENTAL — UNAUDITED research code, built with mainnet support. On mainnet it handles \
     REAL FUNDS, and a mistake in it can lose them";
/// Whether this program was built to accept mainnet. Its own decision: the
/// libraries below may have been built with mainnet for another program.
const MAINNET_ENABLED: bool = cfg!(feature = "mainnet");
const SEED_VARIABLE: &str = "SILENT_WALLET_SEED";
/// A BIP39 seed phrase, as an alternative to the hexadecimal seed.
const PHRASE_VARIABLE: &str = "SILENT_WALLET_PHRASE";
/// The optional BIP39 passphrase that goes with the phrase.
const PASSPHRASE_VARIABLE: &str = "SILENT_WALLET_PASSPHRASE";
const DEFAULT_DB: &str = "silent-wallet.sqlite3";
/// What must be typed to send a transaction.
const CONFIRMATION: &str = "SEND";
/// What must be typed to send a transaction on mainnet.
const MAINNET_CONFIRMATION: &str = "SEND REAL BITCOIN";

#[derive(Parser)]
#[command(
    name = "silent-wallet",
    version,
    about = "Experimental silent payments wallet (regtest and signet; mainnet only in a build \
             with the `mainnet` feature)",
    long_about = "EXPERIMENTAL — unaudited research code. Regtest and signet; mainnet only when \
        built with the `mainnet` feature, and then with real funds at your own risk.\n\n\
        The wallet's secret is read from the environment on every run: either a BIP39 seed \
        phrase in SILENT_WALLET_PHRASE (with an optional passphrase in \
        SILENT_WALLET_PASSPHRASE), or a seed of 16 to 64 bytes as hexadecimal digits in \
        SILENT_WALLET_SEED. It is never stored, and never printed except by `new-phrase`, \
        which makes a new phrase and shows it once."
)]
struct Cli {
    /// Network: regtest or signet. `mainnet` is accepted only by a build
    /// with the `mainnet` feature.
    #[arg(long, global = true, default_value = "regtest", value_parser = parse_network)]
    network: Network,
    #[command(subcommand)]
    command: Command,
}

fn parse_network(value: &str) -> Result<Network, String> {
    let network = value.parse::<Network>().map_err(|e| e.to_string())?;
    if network.is_mainnet() && !MAINNET_ENABLED {
        return Err(silent_sync::NetworkError::MainnetRefused.to_string());
    }
    Ok(network)
}

fn confirmation_for(network: Network) -> &'static str {
    if network.is_mainnet() {
        MAINNET_CONFIRMATION
    } else {
        CONFIRMATION
    }
}

/// The most one transaction may pay: what was asked for, and on mainnet a
/// small default when nothing was.
fn amount_limit(network: Network, asked: Option<u64>) -> Option<u64> {
    asked.or(network
        .is_mainnet()
        .then_some(MAINNET_DEFAULT_MAX_AMOUNT_SATS))
}

#[derive(clap::Args)]
struct DbArg {
    /// Path of the wallet's store. Keep it in a directory only you can write.
    #[arg(long, default_value = DEFAULT_DB)]
    db: PathBuf,
}

#[derive(clap::Args)]
struct NodeArg {
    /// REST URL of your own Bitcoin Core node (started with -rest=1), e.g.
    /// http://127.0.0.1:18443. Loopback only.
    #[arg(long)]
    core_rest_url: String,
}

#[derive(Subcommand)]
enum Command {
    /// Make a new BIP39 seed phrase and print it, once. Write it down: it is
    /// the wallet, and it is not stored anywhere.
    NewPhrase {
        /// Number of words: 12 or 24.
        #[arg(long, default_value_t = 12, value_parser = clap::value_parser!(u8).range(12..=24))]
        words: u8,
    },
    /// Print the address to receive at.
    Address {
        /// A labelled address (the store must scan for the label). Label 0
        /// is the change label and is not handed out.
        #[arg(long)]
        label: Option<u32>,
    },
    /// Create the wallet's store and bind it to these keys, this network and
    /// your node.
    Init {
        #[command(flatten)]
        db: DbArg,
        #[command(flatten)]
        node: NodeArg,
        /// First block height to scan [default: the node's current tip].
        /// Payments confirmed below it are never found.
        #[arg(long)]
        birthday: Option<u32>,
        /// Label integer to scan for in addition to the change label
        /// (repeatable).
        #[arg(long = "label")]
        labels: Vec<u32>,
    },
    /// Scan new blocks from your node; follows reorganizations.
    Sync {
        #[command(flatten)]
        db: DbArg,
        #[command(flatten)]
        node: NodeArg,
        /// Print the report as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Show what the store holds as of its last scanned block.
    Status {
        #[command(flatten)]
        db: DbArg,
        /// Confirmations an output needs to be listed as unspent.
        #[arg(long, default_value_t = 1)]
        min_conf: u32,
        /// Print as JSON on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Build and sign a payment, show it, and — after you confirm — hand it
    /// to your node.
    Send(SendArgs),
    /// Transactions that were sent and are not in a scanned block yet.
    Pending {
        #[command(flatten)]
        db: DbArg,
        /// Forget this transaction (one that will never confirm): its inputs
        /// count as unspent again.
        #[arg(long)]
        forget: Option<Txid>,
    },
}

#[derive(clap::Args)]
struct SendArgs {
    #[command(flatten)]
    db: DbArg,
    #[command(flatten)]
    node: NodeArg,
    /// Address to pay: a silent payment address or an ordinary one, for the
    /// selected network.
    #[arg(long)]
    to: String,
    /// Amount in satoshi.
    #[arg(long)]
    amount: u64,
    /// Refuse to pay more than this many satoshi in one transaction
    /// [default: 100000 on mainnet, no limit on regtest and signet].
    #[arg(long)]
    max_amount: Option<u64>,
    /// Fee rate in satoshi per virtual byte.
    #[arg(long)]
    fee_rate: u64,
    /// Refuse a transaction whose fee exceeds this many satoshi.
    #[arg(long, default_value_t = 100_000)]
    max_fee: u64,
    /// Refuse a transaction whose fee exceeds this percentage of the amount.
    #[arg(long, default_value_t = 50)]
    max_fee_percent: u64,
    /// Confirmations an output needs to be spent.
    #[arg(long, default_value_t = 1)]
    min_conf: u32,
    /// JSON-RPC URL of the node [default: the REST URL; Core serves both on
    /// one port].
    #[arg(long)]
    rpc_url: Option<String>,
    /// The node's cookie file (in its data directory), read to authenticate
    /// the one call that sends the transaction. Never printed.
    #[arg(long, required_unless_present = "no_broadcast")]
    rpc_cookie_file: Option<PathBuf>,
    /// Do not send: print the signed transaction and stop. The printed
    /// transaction is valid: whoever has it can send it. It is not recorded
    /// as pending.
    #[arg(long)]
    no_broadcast: bool,
    /// Print the result as JSON on stdout.
    #[arg(long)]
    json: bool,
}

struct Failure(String);

fn fail(e: impl std::fmt::Display) -> Failure {
    Failure(e.to_string())
}

/// Writes to stdout in one go; a closed stdout is an error, not a panic.
fn emit(text: &str) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Failure(format!("cannot write to stdout: {e}")))
}

fn emit_json(value: &impl serde::Serialize) -> Result<(), Failure> {
    let json = serde_json::to_string_pretty(value).map_err(fail)?;
    emit(&(json + "\n"))
}

fn main() -> ExitCode {
    eprintln!("{EXPERIMENTAL_BANNER}");
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure(message)) => {
            eprintln!("error: {message}");
            ExitCode::from(1)
        }
    }
}

/// The wallet for the seed in the environment and the given labels.
fn wallet(network: Network, labels: &[u32]) -> Result<Wallet, Failure> {
    let keys = match (
        std::env::var(PHRASE_VARIABLE).ok(),
        std::env::var(SEED_VARIABLE).ok(),
    ) {
        (Some(_), Some(_)) => {
            return Err(Failure(format!(
                "both {PHRASE_VARIABLE} and {SEED_VARIABLE} are set; set exactly one of them"
            )))
        }
        (Some(phrase), None) => {
            let passphrase = std::env::var(PASSPHRASE_VARIABLE).unwrap_or_default();
            SeedPhrase::parse(&phrase)
                .and_then(|phrase| phrase.keys(&passphrase, network))
                .map_err(|e| Failure(format!("{PHRASE_VARIABLE}: {e}")))?
        }
        (None, Some(seed)) => WalletKeys::from_seed_hex(&seed, network)
            .map_err(|e| Failure(format!("{SEED_VARIABLE}: {e}")))?,
        (None, None) => {
            return Err(Failure(format!(
                "the environment variable {SEED_VARIABLE} is not set, and neither is \
                 {PHRASE_VARIABLE}. One of them must hold the wallet's secret: a BIP39 seed \
                 phrase in {PHRASE_VARIABLE} (`silent-wallet new-phrase` makes one), or 16 to 64 \
                 bytes as hexadecimal digits in {SEED_VARIABLE}. This program does not store \
                 either"
            )))
        }
    };
    Wallet::new(keys, network, labels).map_err(fail)
}

fn open_store(network: Network, db: &Path) -> Result<(SqliteStore, StoreMeta), Failure> {
    let store = SqliteStore::open(db).map_err(|e| {
        Failure(format!(
            "{e}\ncannot open the store {}; create it with `silent-wallet init`",
            db.display()
        ))
    })?;
    let meta = store.meta().map_err(fail)?.ok_or_else(|| {
        Failure(format!(
            "{} has no binding; delete it and run `init`",
            db.display()
        ))
    })?;
    if meta.chain.network() != network {
        return Err(Failure(format!(
            "store {} is for {}, but --network is {network}",
            db.display(),
            meta.chain.network()
        )));
    }
    Ok((store, meta))
}

/// The wallet for the store's label set; fails if the seed in the
/// environment is not the one the store was created with.
fn wallet_of(network: Network, meta: &StoreMeta) -> Result<Wallet, Failure> {
    let extra: Vec<u32> = meta.labels.iter().copied().filter(|&m| m != 0).collect();
    let wallet = wallet(network, &extra)?;
    if wallet.scanner().wallet_id() != meta.wallet_id {
        return Err(Failure(format!(
            "the seed in the environment ({SEED_VARIABLE} or {PHRASE_VARIABLE}) is not the one \
             this store was created with"
        )));
    }
    Ok(wallet)
}

fn pending_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(".pending.json");
    name.into()
}

fn run(cli: Cli) -> Result<(), Failure> {
    let network = cli.network;
    match cli.command {
        Command::NewPhrase { words } => {
            let count = match words {
                12 => WordCount::Twelve,
                24 => WordCount::TwentyFour,
                _ => return Err(Failure("--words must be 12 or 24".to_string())),
            };
            let phrase = SeedPhrase::generate(count).map_err(fail)?;
            eprintln!(
                "This is the wallet: whoever has these words can spend from it, and without \
                 them it cannot be restored.\nWrite them down now. They are shown once and not \
                 stored anywhere. To use the wallet, put them in the environment variable \
                 {PHRASE_VARIABLE}."
            );
            emit(&format!("{}\n", phrase.reveal()))
        }
        Command::Address { label } => {
            // (The library refuses label 0, the change label.)
            let address = wallet(network, &[])?.address(label).map_err(fail)?;
            if let Some(m) = label {
                eprintln!(
                    "note: payments to this address are only found by a store that scans for \
                     label {m} (`init --label {m}`). Keep a note of the labels you use."
                );
            }
            emit(&format!("{address}\n"))
        }
        Command::Init {
            db,
            node,
            birthday,
            labels,
        } => init(network, &db.db, &node.core_rest_url, birthday, &labels),
        Command::Sync { db, node, json } => {
            let (mut store, meta) = open_store(network, &db.db)?;
            let wallet = wallet_of(network, &meta)?;
            let source = CoreRestSource::connect(&node.core_rest_url).map_err(fail)?;
            let report = sync_store(&wallet, &source, &mut store)?;
            let settled = settle_pending(&db.db, &store)?;
            if json {
                emit_json(&report)
            } else {
                emit(&render_sync(&report, &settled))
            }
        }
        Command::Status { db, min_conf, json } => {
            let (store, _) = open_store(network, &db.db)?;
            let pending = PendingSpends::load(&pending_path(&db.db)).map_err(fail)?;
            let held = holdings(&store, min_conf, &pending).map_err(fail)?;
            if json {
                emit_json(&held)
            } else {
                emit(&render_holdings(&held))
            }
        }
        Command::Send(args) => send(network, args),
        Command::Pending { db, forget } => {
            let path = pending_path(&db.db);
            let mut pending = PendingSpends::load(&path).map_err(fail)?;
            if let Some(txid) = forget {
                if !pending.forget(txid) {
                    return Err(Failure(format!("{txid} is not on record")));
                }
                pending.save(&path).map_err(fail)?;
                eprintln!("forgot {txid}; its inputs count as unspent again");
            }
            let mut out = String::new();
            for t in pending.transactions() {
                let state = match t.confirmed_at {
                    Some(height) => format!("in a block at height {height}"),
                    None => "waiting for a block".to_string(),
                };
                let _ = writeln!(
                    out,
                    "{}  {} input(s), sent when the store was at height {}, {state}",
                    t.txid,
                    t.inputs.len(),
                    t.sent_at_height
                );
            }
            if pending.inputs().is_empty() {
                out.push_str("no transaction is waiting for a block\n");
            }
            emit(&out)
        }
    }
}

fn init(
    network: Network,
    db: &Path,
    core_rest_url: &str,
    birthday: Option<u32>,
    labels: &[u32],
) -> Result<(), Failure> {
    let wallet = wallet(network, labels)?;
    let source = CoreRestSource::connect(core_rest_url).map_err(fail)?;
    if source.chain().network() != network {
        return Err(Failure(format!(
            "the node is on {}, but --network is {network}",
            source.chain().network()
        )));
    }
    let tip = source.tip().map_err(fail)?.height;
    let (birthday, basis) = match birthday {
        Some(height) => (height, "given when the wallet was created".to_string()),
        None => (
            tip.max(1),
            format!("the wallet was created when the node's tip was {tip}"),
        ),
    };
    let meta = new_store_meta(&source, wallet.scanner(), birthday, &basis);
    let mut store = SqliteStore::create(db).map_err(fail)?;
    if let Err(e) = store.initialize(&meta) {
        drop(store);
        let _ = std::fs::remove_file(db);
        return Err(fail(e));
    }
    eprintln!("created {}", db.display());
    eprintln!(
        "note: payments confirmed below height {birthday} will never be found by this store."
    );
    emit(&format!(
        "store   : {}\nnetwork : {network}\nbirthday: {birthday}\nlabels  : {:?}\naddress : {}\n",
        db.display(),
        meta.labels,
        wallet.address(None).map_err(fail)?
    ))
}

fn sync_store(
    wallet: &Wallet,
    source: &CoreRestSource,
    store: &mut SqliteStore,
) -> Result<SyncReport, Failure> {
    let mut on_commit = |checkpoint: silent_sync::BlockRef| {
        eprintln!("scanned up to height {}", checkpoint.height);
    };
    match sync_with_progress(
        source,
        wallet.scanner(),
        store,
        &SyncOptions::default(),
        &mut on_commit,
    ) {
        Ok(report) => Ok(report),
        Err(SyncError::Interrupted { cause, partial }) => Err(Failure(format!(
            "{cause}\nsync stopped after scanning up to height {}; the store is consistent, run \
             sync again",
            partial
                .checkpoint
                .map_or_else(|| "none".to_string(), |cp| cp.height.to_string())
        ))),
        Err(e) => Err(Failure(format!(
            "{e}\nsync aborted: the store was not changed"
        ))),
    }
}

/// Drops pending transactions that a scanned block has settled. Returns
/// lines describing what happened to them.
fn settle_pending(db: &Path, store: &SqliteStore) -> Result<Vec<String>, Failure> {
    let path = pending_path(db);
    let mut pending = PendingSpends::load(&path).map_err(fail)?;
    if pending.transactions().is_empty() {
        return Ok(Vec::new());
    }
    let before = pending.clone();
    let checkpoint = store
        .checkpoint()
        .map_err(fail)?
        .map_or(0, |block| block.height);
    let changes = pending.settle(&store.spends().map_err(fail)?, checkpoint);
    if pending != before {
        pending.save(&path).map_err(fail)?;
    }
    Ok(changes
        .into_iter()
        .map(|(t, change)| match change {
            Settlement::Confirmed => format!("sent transaction {} is in a block", t.txid),
            Settlement::Unconfirmed => format!(
                "sent transaction {} is NOT in a block any more (the chain was reorganized); it \
                 is waiting again and its inputs are not offered to another transaction",
                t.txid
            ),
            Settlement::Conflicted => format!(
                "sent transaction {} can no longer confirm: another transaction spent one of \
                 its inputs",
                t.txid
            ),
        })
        .collect())
}

fn render_sync(report: &SyncReport, settled: &[String]) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    if let Some(reorg) = &report.reorg {
        line(format!(
            "REORG: rolled back to height {} ({} blocks; {} detected outputs removed, {} spends \
             reverted)",
            reorg.fork_height,
            reorg.blocks_rolled_back,
            reorg.detections_removed.len(),
            reorg.spends_reverted.len()
        ));
    }
    line(format!("blocks scanned : {}", report.blocks_scanned));
    line(format!(
        "checkpoint     : {}",
        report
            .checkpoint
            .map_or_else(|| "none".to_string(), |cp| format!("height {}", cp.height))
    ));
    line(format!("newly detected : {}", report.new_detections.len()));
    for d in &report.new_detections {
        line(output_line(d, None));
    }
    line(format!("newly spent    : {}", report.new_spends.len()));
    for s in &report.new_spends {
        line(format!(
            "  {}:{} spent at height {} by {}",
            s.txid, s.vout, s.spent_in.height, s.spending_txid
        ));
    }
    line(format!(
        "caught up      : {}",
        if report.caught_up { "yes" } else { "no" }
    ));
    for text in settled {
        line(text.clone());
    }
    out
}

fn output_line(d: &DetectedOutput, confirmations: Option<u32>) -> String {
    let label = d.label.map_or_else(|| "-".to_string(), |m| m.to_string());
    let confirmed = confirmations.map_or_else(String::new, |n| format!("  {n} conf"));
    format!(
        "  height {:>6}  {}:{}  {} sat  label {}{}",
        d.block.height, d.txid, d.vout, d.value_sats, label, confirmed
    )
}

fn render_holdings(held: &Holdings) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    let Some(as_of) = held.as_of else {
        line("nothing scanned yet; run `sync`".to_string());
        return out;
    };
    line(format!(
        "as of block      : height {} ({})",
        as_of.height, as_of.hash
    ));
    line(format!(
        "unspent          : {} sat in {} output(s)",
        held.unspent_sats(),
        held.unspent.len()
    ));
    line(format!(
        "                   (detected in full blocks from your node, no spend recorded up to \
         that block, at least {} confirmation(s))",
        held.min_confirmations
    ));
    for h in &held.unspent {
        line(output_line(&h.output, Some(h.confirmations)));
    }
    if !held.unconfirmed_enough.is_empty() {
        line(format!(
            "waiting for confirmations: {} output(s), not counted above",
            held.unconfirmed_enough.len()
        ));
        for h in &held.unconfirmed_enough {
            line(output_line(&h.output, Some(h.confirmations)));
        }
    }
    if !held.spent_by_pending.is_empty() {
        line(format!(
            "spent by transactions you sent that are not in a block yet: {} output(s), not \
             counted above",
            held.spent_by_pending.len()
        ));
        for h in &held.spent_by_pending {
            line(output_line(&h.output, Some(h.confirmations)));
        }
    }
    line(format!("spent (recorded) : {} output(s)", held.spent));
    if !held.index_claims.is_empty() {
        line(format!(
            "claimed by an index, NOT verified and NOT counted: {} output(s)",
            held.index_claims.len()
        ));
        for d in &held.index_claims {
            line(output_line(d, None));
        }
    }
    line(
        "note: transactions that are not in a block yet are not seen. This is a statement about \
         the chain your node showed up to the block above, nothing more."
            .to_string(),
    );
    out
}

fn render_built(built: &BuiltTransaction) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    line(format!("transaction : {}", built.txid));
    line(format!("spends      : {} output(s)", built.inputs.len()));
    for input in &built.inputs {
        line(format!("  {}  {} sat", input.outpoint, input.value_sats));
    }
    for paid in &built.payments {
        line(format!(
            "pays        : {} sat to {}",
            paid.amount_sats, paid.to
        ));
    }
    match &built.change {
        Some(change) => line(format!(
            "change      : {} sat back to this wallet (output {})",
            change.amount_sats, change.vout
        )),
        None => line("change      : none".to_string()),
    }
    line(format!(
        "fee         : {} sat ({} vB; {} sat/vB asked for, {:.2} sat/vB paid)",
        built.fee_sats,
        built.vsize,
        built.fee_rate_asked,
        built.fee_sats as f64 / built.vsize as f64
    ));
    let surplus = built
        .fee_sats
        .saturating_sub(built.vsize * built.fee_rate_asked);
    if surplus > 0 {
        line(format!(
            "              (includes {surplus} sat that were too little to return as change)"
        ));
    }
    out
}

#[derive(serde::Serialize)]
struct SendJson<'a> {
    #[serde(flatten)]
    built: &'a BuiltTransaction,
    raw_transaction_hex: String,
    broadcast: bool,
}

fn send(network: Network, args: SendArgs) -> Result<(), Failure> {
    if let Some(limit) = amount_limit(network, args.max_amount) {
        if args.amount > limit {
            return Err(Failure(format!(
                "{} sat is more than the limit of {limit} sat for one transaction on {network} \
                 (raise it with --max-amount)\nnothing was built or sent",
                args.amount
            )));
        }
    }
    let (mut store, meta) = open_store(network, &args.db.db)?;
    let wallet = wallet_of(network, &meta)?;
    let destination = Destination::parse(&args.to, network).map_err(fail)?;
    let source = CoreRestSource::connect(&args.node.core_rest_url).map_err(fail)?;
    if source.capabilities().evidence_kind != EvidenceKind::FullBlockWithPrevouts {
        return Err(Failure("the node does not supply full blocks".to_string()));
    }
    // Set the RPC connection up before anything is built: a missing cookie
    // should not be discovered after the confirmation.
    let rpc = match (&args.rpc_cookie_file, args.no_broadcast) {
        (_, true) => None,
        (Some(cookie), false) => {
            let rpc = CoreRpc::new(
                args.rpc_url.as_deref().unwrap_or(&args.node.core_rest_url),
                cookie,
            )
            .map_err(fail)?;
            // The node that will be handed the transaction must be on the
            // store's chain — not, say, a mainnet node on that port.
            rpc.check_chain(meta.chain.genesis_hash())
                .map_err(|e| Failure(format!("{e}\nnothing was built or sent")))?;
            Some(rpc)
        }
        (None, false) => {
            return Err(Failure(
                "--rpc-cookie-file is required to send (or use --no-broadcast)".to_string(),
            ))
        }
    };

    // Spend only what the chain shows now: bring the store up to the node.
    let report = sync_store(&wallet, &source, &mut store)?;
    if !report.caught_up {
        return Err(Failure(
            "the store is not at the node's tip; run `sync` and try again".to_string(),
        ));
    }
    for text in settle_pending(&args.db.db, &store)? {
        eprintln!("{text}");
    }
    let pending_file = pending_path(&args.db.db);
    let pending = PendingSpends::load(&pending_file).map_err(fail)?;
    let held = holdings(&store, args.min_conf, &pending).map_err(fail)?;
    let candidates: Vec<DetectedOutput> = held.unspent.iter().map(|h| h.output.clone()).collect();
    let limits = SendLimits::default()
        .with_max_fee_sats(args.max_fee)
        .with_max_fee_percent(args.max_fee_percent);
    let built = build_and_sign(
        &wallet,
        &source,
        &candidates,
        &[Payment {
            to: destination,
            amount_sats: args.amount,
        }],
        args.fee_rate,
        &limits,
    )
    .map_err(|e| Failure(format!("{e}\nnothing was sent")))?;
    let raw = serialize_hex(&built.transaction);
    let show = |broadcast: bool| -> Result<(), Failure> {
        if args.json {
            emit_json(&SendJson {
                built: &built,
                raw_transaction_hex: raw.clone(),
                broadcast,
            })
        } else {
            let state = if broadcast {
                "sent to your node"
            } else {
                "NOT sent"
            };
            emit(&format!(
                "{}status      : {state}\nraw         : {raw}\n",
                render_built(&built)
            ))
        }
    };

    let Some(rpc) = rpc else {
        eprintln!(
            "--no-broadcast: the transaction was signed and is printed, not sent, and not \
             recorded as pending. Whoever has this text can send it."
        );
        return show(false);
    };
    // The decision to send is the user's, on what is shown here.
    let confirmation = confirmation_for(network);
    let stakes = if network.is_mainnet() {
        "\nThis is MAINNET: the transaction moves real bitcoin, with unaudited software."
    } else {
        ""
    };
    eprint!(
        "{}\nThis hands the transaction to your {network} node; it cannot be taken back.{stakes}\n\
         Type {confirmation} to send it: ",
        render_built(&built)
    );
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(fail)?;
    if answer.trim() != confirmation {
        return Err(Failure("not confirmed; nothing was sent".to_string()));
    }
    if let Err(e) = rpc.would_accept(&built.transaction) {
        // The node has it already: it went out before. That is not "nothing
        // was sent"; put it on record so that its inputs are not used again.
        if e.means_already_with_node() {
            let txid = built.txid;
            let mut pending = PendingSpends::load(&pending_file).map_err(fail)?;
            pending.record(PendingTransaction {
                txid,
                inputs: built.inputs.iter().map(|i| i.outpoint).collect(),
                sent_at_height: held.as_of.map_or(0, |b| b.height),
                confirmed_at: None,
            });
            pending.save(&pending_file).map_err(fail)?;
            return Err(Failure(format!(
                "your node already has transaction {txid}: it was sent before. Nothing new was \
                 sent; it is recorded as pending"
            )));
        }
        return Err(Failure(format!("{e}\nnothing was sent")));
    }

    // Record the transaction as pending *before* it leaves: if the answer is
    // lost, or this process dies, its inputs must not be offered again. The
    // record is read afresh, in case something else changed it meanwhile.
    let txid = built.txid;
    let mut pending = PendingSpends::load(&pending_file).map_err(fail)?;
    pending.record(PendingTransaction {
        txid,
        inputs: built.inputs.iter().map(|i| i.outpoint).collect(),
        sent_at_height: held.as_of.map_or(0, |b| b.height),
        confirmed_at: None,
    });
    pending.save(&pending_file).map_err(|e| {
        Failure(format!(
            "{e}\nthe transaction could not be recorded as pending; nothing was sent"
        ))
    })?;
    match rpc.send(&built.transaction) {
        Ok(_) => {}
        // The node said no, or was never asked: take the record back.
        Err(e) if e.means_not_done() => {
            pending.forget(txid);
            let note = match pending.save(&pending_file) {
                Ok(()) => String::new(),
                Err(save) => format!(
                    "\n(it is still listed as pending: {save}; remove it with `pending --forget \
                     {txid}`)"
                ),
            };
            return Err(Failure(format!("{e}\nnothing was sent{note}")));
        }
        Err(e) => {
            return Err(Failure(format!(
                "{e}\nIT IS NOT KNOWN whether your node took transaction {txid}.\n\
                 It stays recorded as pending, so its inputs are not offered again. Do NOT repeat \
                 the payment until you know: look for it in the node's mempool, or run `sync` \
                 after the next block. If it never appears, `silent-wallet pending --forget \
                 {txid}` makes its inputs available again."
            )))
        }
    }
    show(true)
        .map_err(|Failure(message)| Failure(format!("{message}\nthe transaction {txid} was sent")))
}
