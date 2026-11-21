//! Measurements for `silent-sync bench` and their export.
//!
//! What is measured is this process only: the time and CPU the *client* spends
//! and the bytes it downloads. Work a server did beforehand (a node validating
//! blocks, an index server computing tweaks) is not in these numbers and is
//! reported as "not measured", never as zero.
//!
//! A results file holds no personal data: no transaction ids, no outputs, no
//! wallet identifier, no file paths, no URLs. It holds block hashes of the
//! measured range, which are public chain data.

use lab_storage::SqliteStore;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    time::Instant,
};
use sync_core::{
    bitcoin::OutPoint,
    engine::scan_range,
    models::DetectedOutput,
    network::{ChainIdentity, LabNetwork},
    protocol::ReceiveScanner,
    source::{ScanSource, SourceStats},
    store::{SyncStore, SCANNER_VERSION},
    sync::{new_store_meta, sync, SyncOptions},
};

pub const FORMAT: &str = "silent-sync-lab/benchmark-v1";

/// What one run measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Stateless scan of the whole range; nothing is stored.
    Scan,
    /// New store, synced from the first height of the range to the last.
    SyncCold,
    /// A store already synced to the middle of the range; the rest is measured.
    SyncResume,
    /// A store already synced to the end of the range; a sync that finds
    /// nothing to do is measured.
    SyncNoop,
    /// The store is deleted and rebuilt from its birthday; the result is
    /// compared with what the deleted store held.
    Restore,
}

impl Mode {
    pub const ALL: [Mode; 5] = [
        Mode::Scan,
        Mode::SyncCold,
        Mode::SyncResume,
        Mode::SyncNoop,
        Mode::Restore,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Scan => "scan",
            Mode::SyncCold => "sync_cold",
            Mode::SyncResume => "sync_resume",
            Mode::SyncNoop => "sync_noop",
            Mode::Restore => "restore",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Mode::ALL.into_iter().find(|m| m.as_str() == text)
    }

    /// What was and was not warm when the measured part started.
    fn cache_condition(self) -> &'static str {
        match self {
            Mode::Scan => "client cold: new source connection, no store; server caches not controlled",
            Mode::SyncCold => "client cold: new source connection, new empty store; server caches not controlled",
            Mode::SyncResume => "resume: new source connection, existing store synced to the middle of the range; server warm from the preparation",
            Mode::SyncNoop => "warm: new source connection, existing store already at the end of the range",
            Mode::Restore => "restore: store deleted, new store from the same birthday; server warm from the preparation",
        }
    }
}

/// How one run's result relates to the first successful run of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// This run defines the reference detection set.
    Reference,
    SameAsReference,
    DiffersFromReference,
    /// The run failed; there is nothing to compare.
    NotRun,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchRecord {
    pub run_id: String,
    /// Position in the order the runs were executed (1-based).
    pub order: u32,
    /// Backend as named on the command line.
    pub backend: String,
    /// Identifier the source reports for itself (empty if the run failed
    /// before a source was opened).
    pub backend_id: String,
    pub mode: Mode,
    pub network: LabNetwork,
    pub evidence_kind: String,
    pub from_height: u32,
    pub to_height: u32,
    pub first_block_hash: String,
    pub last_block_hash: String,
    /// First height processed inside the measured part (differs from
    /// `from_height` for `sync_resume`; above `to_height` for `sync_noop`).
    pub measured_from_height: u32,
    /// 1-based repetition number.
    pub repeat: u32,
    pub cache_condition: String,
    /// Wall-clock time of the measured part, including `connect_ms`.
    pub wall_ms: f64,
    /// Part of `wall_ms` spent opening the source (and, for network backends,
    /// its first requests).
    pub connect_ms: f64,
    /// CPU time of this process during the measured part; `null` where the
    /// platform does not offer it. Resolution is one clock tick.
    pub cpu_ms: Option<f64>,
    /// Requests sent and response bytes received during the measured part.
    pub requests: u64,
    pub downloaded_bytes: u64,
    pub blocks: u64,
    /// Transactions looked at (candidates).
    pub txs_seen: u64,
    /// Transactions that were eligible and actually scanned.
    pub txs_scanned: u64,
    pub txs_skipped: BTreeMap<String, u64>,
    pub detected_count: u64,
    pub labels_scanned: Vec<u32>,
    /// Time the server spent preparing the data. Not obtainable from any
    /// backend so far: always `null`, never an estimate.
    pub server_preprocess_ms: Option<f64>,
    pub verification_status: Verification,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub tool_version: String,
    pub scanner_version: String,
    pub rustc: String,
    pub target: String,
    pub build_profile: String,
    pub os: String,
    pub arch: String,
    pub logical_cpus: Option<u32>,
    /// Free-form notes given on the command line (server versions, machine).
    pub notes: Vec<String>,
}

impl Environment {
    pub fn capture(notes: Vec<String>) -> Self {
        Self {
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            scanner_version: SCANNER_VERSION.to_string(),
            rustc: env!("LAB_RUSTC_VERSION").to_string(),
            target: env!("LAB_TARGET").to_string(),
            build_profile: if cfg!(debug_assertions) {
                "debug (unoptimized workspace crates; not representative)"
            } else {
                "release"
            }
            .to_string(),
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            logical_cpus: std::thread::available_parallelism()
                .ok()
                .and_then(|n| u32::try_from(n.get()).ok()),
            notes,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchFile {
    pub format: String,
    pub environment: Environment,
    pub runs: Vec<BenchRecord>,
}

/// CPU time used by this process so far, in milliseconds. Linux only (from
/// `/proc/self/stat`, in clock ticks whose length `getconf CLK_TCK` reports);
/// `None` elsewhere or when either cannot be read.
pub struct CpuClock {
    ticks_per_second: Option<f64>,
}

impl CpuClock {
    pub fn new() -> Self {
        let ticks_per_second = std::process::Command::new("getconf")
            .arg("CLK_TCK")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<f64>().ok())
            .filter(|t| *t > 0.0);
        Self { ticks_per_second }
    }

    pub fn now_ms(&self) -> Option<f64> {
        let per_second = self.ticks_per_second?;
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        // The second field (the command name) may contain spaces; everything
        // after the closing parenthesis is space separated. utime and stime
        // are the 14th and 15th fields overall.
        let rest = stat.rsplit_once(')')?.1;
        let mut fields = rest.split_whitespace().skip(11);
        let utime: f64 = fields.next()?.parse().ok()?;
        let stime: f64 = fields.next()?.parse().ok()?;
        Some((utime + stime) * 1_000.0 / per_second)
    }
}

impl Default for CpuClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Opens a fresh connection to one backend.
pub type OpenSource<'a> = &'a dyn Fn() -> Result<Box<dyn ScanSource>, String>;

pub struct BenchPlan<'a> {
    pub network: LabNetwork,
    /// (name used on the command line, how to open it)
    pub backends: Vec<(String, OpenSource<'a>)>,
    pub modes: Vec<Mode>,
    pub from: u32,
    pub to: u32,
    pub repeat: u32,
    /// Directory in which temporary stores are created (and removed).
    pub work_dir: PathBuf,
}

/// A temporary store file, removed when dropped.
struct TempStore {
    path: PathBuf,
}

impl TempStore {
    fn new(work_dir: &Path, order: u32) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let name = format!(
            "silent-sync-bench-{}-{order}-{nonce:09}.sqlite3",
            std::process::id()
        );
        Self {
            path: work_dir.join(name),
        }
    }

    fn remove(&self) {
        let _ = std::fs::remove_file(&self.path);
        // SQLite's rollback journal, should a failed run have left one.
        let mut journal = self.path.clone().into_os_string();
        journal.push("-journal");
        let _ = std::fs::remove_file(journal);
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        self.remove();
    }
}

type DetectionKey = (u32, OutPoint, u64, Option<u32>);

fn keys(detections: &[DetectedOutput]) -> BTreeSet<DetectionKey> {
    detections
        .iter()
        .map(|d| {
            (
                d.block.height,
                OutPoint::new(d.txid, d.vout),
                d.value_sats,
                d.label,
            )
        })
        .collect()
}

/// What the measured part of one run produced.
struct Measured {
    backend: String,
    evidence_kind: String,
    first_block_hash: String,
    last_block_hash: String,
    measured_from_height: u32,
    wall_ms: f64,
    connect_ms: f64,
    cpu_ms: Option<f64>,
    stats: SourceStats,
    blocks: u64,
    txs_seen: u64,
    txs_scanned: u64,
    txs_skipped: BTreeMap<String, u64>,
    /// Everything detected in the whole range after the run.
    detections: BTreeSet<DetectionKey>,
    /// `false` when a check specific to the mode failed (restore mismatch).
    mode_check_passed: bool,
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1_000.0
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

struct Runner<'a> {
    cpu: &'a CpuClock,
    network: LabNetwork,
    scanner: &'a ReceiveScanner,
    wallet_id: &'a str,
    from: u32,
    to: u32,
}

impl Runner<'_> {
    fn chain(&self) -> ChainIdentity {
        ChainIdentity::for_network(self.network)
    }

    fn options(&self, target: u32) -> SyncOptions {
        SyncOptions {
            target_height: Some(target),
            ..SyncOptions::default()
        }
    }

    /// Creates a store bound to `source` with the range start as birthday.
    fn new_store(&self, source: &dyn ScanSource, path: &Path) -> Result<SqliteStore, String> {
        let mut store = SqliteStore::create(path).map_err(text)?;
        store
            .initialize(&new_store_meta(
                source,
                self.scanner,
                self.wallet_id,
                self.from,
                "benchmark: first height of the measured range",
            ))
            .map_err(text)?;
        Ok(store)
    }

    /// Not measured: brings a new store at `path` up to `target`.
    fn prepare(&self, open: OpenSource<'_>, path: &Path, target: u32) -> Result<(), String> {
        let source = open()?;
        let mut store = self.new_store(source.as_ref(), path)?;
        sync(
            source.as_ref(),
            self.scanner,
            self.wallet_id,
            &mut store,
            &self.options(target),
        )
        .map_err(text)?;
        Ok(())
    }

    fn run(&self, mode: Mode, open: OpenSource<'_>, store: &TempStore) -> Result<Measured, String> {
        let middle = self.from + (self.to - self.from) / 2;
        let mut original = None;
        match mode {
            Mode::Scan | Mode::SyncCold => {}
            Mode::SyncResume => self.prepare(open, &store.path, middle)?,
            Mode::SyncNoop => self.prepare(open, &store.path, self.to)?,
            Mode::Restore => {
                self.prepare(open, &store.path, self.to)?;
                let held = SqliteStore::open(&store.path)
                    .and_then(|s| s.detections())
                    .map_err(text)?;
                original = Some(keys(&held));
                store.remove();
            }
        }

        // ---- measured part starts with opening the source ----
        let cpu_before = self.cpu.now_ms();
        let started = Instant::now();
        let source = open()?;
        let connect_ms = ms(started);
        let source = source.as_ref();
        let before = source.stats();
        let capabilities = source.capabilities();
        let evidence_kind = serde_json::to_value(capabilities.evidence_kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();

        let (blocks, txs_seen, txs_scanned, skipped, detections, measured_from) = match mode {
            Mode::Scan => {
                let report = scan_range(source, self.scanner, self.chain(), self.from, self.to)
                    .map_err(text)?;
                (
                    report.blocks_scanned,
                    report.txs_seen,
                    report.txs_scanned,
                    report.txs_skipped,
                    keys(&report.detected_received_outputs),
                    self.from,
                )
            }
            Mode::SyncCold | Mode::Restore | Mode::SyncResume | Mode::SyncNoop => {
                let mut db = match mode {
                    Mode::SyncCold | Mode::Restore => self.new_store(source, &store.path)?,
                    _ => SqliteStore::open(&store.path).map_err(text)?,
                };
                let report = sync(
                    source,
                    self.scanner,
                    self.wallet_id,
                    &mut db,
                    &self.options(self.to),
                )
                .map_err(text)?;
                let reached = report.checkpoint.map(|c| c.height);
                if reached != Some(self.to) {
                    return Err(format!(
                        "sync stopped at {reached:?} instead of {} ({:?})",
                        self.to, report.stopped_because
                    ));
                }
                let measured_from = report
                    .resumed_from
                    .map_or(self.from, |c| c.height.saturating_add(1));
                (
                    report.blocks_scanned,
                    report.txs_seen,
                    report.txs_scanned,
                    report.txs_skipped,
                    keys(&db.detections().map_err(text)?),
                    measured_from,
                )
            }
        };
        let after = source.stats();
        let wall_ms = ms(started);
        let cpu_ms = match (cpu_before, self.cpu.now_ms()) {
            (Some(before), Some(after)) => Some(after - before),
            _ => None,
        };
        // ---- measured part ends ----

        let (first, last) = (
            source.block_ref(self.from).map_err(text)?,
            source.block_ref(self.to).map_err(text)?,
        );
        Ok(Measured {
            backend: source.backend_id().to_string(),
            evidence_kind,
            first_block_hash: first.hash.to_string(),
            last_block_hash: last.hash.to_string(),
            measured_from_height: measured_from,
            wall_ms,
            connect_ms,
            cpu_ms,
            stats: SourceStats {
                requests: after.requests - before.requests,
                downloaded_bytes: after.downloaded_bytes - before.downloaded_bytes,
            },
            blocks,
            txs_seen,
            txs_scanned,
            txs_skipped: skipped
                .into_iter()
                .map(|(reason, count)| (reason.to_string(), count))
                .collect(),
            mode_check_passed: original.is_none_or(|held| held == detections),
            detections,
        })
    }
}

/// Runs every backend × mode `repeat` times, round-robin (so that drift over
/// time affects all of them alike), and returns one record per run. A failed
/// run is recorded with its error; it does not stop the others.
pub fn run_plan(
    plan: &BenchPlan<'_>,
    scanner: &ReceiveScanner,
    wallet_id: &str,
    progress: &mut dyn FnMut(&BenchRecord),
) -> Vec<BenchRecord> {
    let cpu = CpuClock::new();
    let runner = Runner {
        cpu: &cpu,
        network: plan.network,
        scanner,
        wallet_id,
        from: plan.from,
        to: plan.to,
    };
    let mut records = Vec::new();
    let mut reference: Option<BTreeSet<DetectionKey>> = None;
    let mut order = 0u32;

    for repeat in 1..=plan.repeat {
        for (name, open) in &plan.backends {
            for &mode in &plan.modes {
                order += 1;
                let store = TempStore::new(&plan.work_dir, order);
                let outcome = runner.run(mode, *open, &store);
                let mut record = BenchRecord {
                    run_id: format!(
                        "{name}-{}-{}-{}-r{repeat}",
                        mode.as_str(),
                        plan.from,
                        plan.to
                    ),
                    order,
                    backend: name.clone(),
                    backend_id: String::new(),
                    mode,
                    network: plan.network,
                    evidence_kind: String::new(),
                    from_height: plan.from,
                    to_height: plan.to,
                    first_block_hash: String::new(),
                    last_block_hash: String::new(),
                    measured_from_height: plan.from,
                    repeat,
                    cache_condition: mode.cache_condition().to_string(),
                    wall_ms: 0.0,
                    connect_ms: 0.0,
                    cpu_ms: None,
                    requests: 0,
                    downloaded_bytes: 0,
                    blocks: 0,
                    txs_seen: 0,
                    txs_scanned: 0,
                    txs_skipped: BTreeMap::new(),
                    detected_count: 0,
                    labels_scanned: scanner.labels().collect(),
                    server_preprocess_ms: None,
                    verification_status: Verification::NotRun,
                    error: None,
                };
                match outcome {
                    Ok(measured) => {
                        record.backend_id = measured.backend;
                        record.evidence_kind = measured.evidence_kind;
                        record.first_block_hash = measured.first_block_hash;
                        record.last_block_hash = measured.last_block_hash;
                        record.measured_from_height = measured.measured_from_height;
                        record.wall_ms = measured.wall_ms;
                        record.connect_ms = measured.connect_ms;
                        record.cpu_ms = measured.cpu_ms;
                        record.requests = measured.stats.requests;
                        record.downloaded_bytes = measured.stats.downloaded_bytes;
                        record.blocks = measured.blocks;
                        record.txs_seen = measured.txs_seen;
                        record.txs_scanned = measured.txs_scanned;
                        record.txs_skipped = measured.txs_skipped;
                        record.detected_count = measured.detections.len() as u64;
                        record.verification_status = match &reference {
                            None => {
                                reference = Some(measured.detections);
                                Verification::Reference
                            }
                            Some(expected) if *expected == measured.detections => {
                                Verification::SameAsReference
                            }
                            Some(_) => Verification::DiffersFromReference,
                        };
                        if !measured.mode_check_passed {
                            record.verification_status = Verification::DiffersFromReference;
                            record.error = Some(
                                "the restored store differs from the store it replaced".to_string(),
                            );
                        }
                    }
                    Err(e) => record.error = Some(single_line(&e)),
                }
                progress(&record);
                records.push(record);
            }
        }
    }
    records
}

/// Error text made safe for a one-line field: no control characters, bounded.
pub fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect::<String>()
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Summaries and export
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub backend: String,
    pub mode: Mode,
    pub from_height: u32,
    pub to_height: u32,
    pub runs: u32,
    pub errors: u32,
    pub mismatches: u32,
    pub wall_ms_median: Option<f64>,
    pub wall_ms_min: Option<f64>,
    pub wall_ms_max: Option<f64>,
    /// (max − min) / median, in percent.
    pub wall_spread_percent: Option<f64>,
    pub cpu_ms_median: Option<f64>,
    pub downloaded_bytes_median: Option<f64>,
    pub requests_median: Option<f64>,
    pub blocks: Option<u64>,
    pub txs_seen: Option<u64>,
    pub txs_scanned: Option<u64>,
    pub detected_count: Option<u64>,
}

/// Median of the values (mean of the two middle ones for an even count).
pub fn median(values: &[f64]) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[mid]
    } else {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    })
}

pub fn summarize(runs: &[BenchRecord]) -> Vec<Summary> {
    let mut groups: BTreeMap<(u32, u32, String, Mode), Vec<&BenchRecord>> = BTreeMap::new();
    for r in runs {
        groups
            .entry((
                r.to_height - r.from_height,
                r.from_height,
                r.backend.clone(),
                r.mode,
            ))
            .or_default()
            .push(r);
    }
    groups
        .into_values()
        .filter_map(|group| {
            let first = *group.first()?;
            let good: Vec<&BenchRecord> = group
                .iter()
                .copied()
                .filter(|r| r.error.is_none())
                .collect();
            let wall: Vec<f64> = good.iter().map(|r| r.wall_ms).collect();
            let cpu: Vec<f64> = good.iter().filter_map(|r| r.cpu_ms).collect();
            let bytes: Vec<f64> = good.iter().map(|r| r.downloaded_bytes as f64).collect();
            let requests: Vec<f64> = good.iter().map(|r| r.requests as f64).collect();
            let wall_median = median(&wall);
            let min = wall.iter().copied().reduce(f64::min);
            let max = wall.iter().copied().reduce(f64::max);
            let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
            Some(Summary {
                backend: first.backend.clone(),
                mode: first.mode,
                from_height: first.from_height,
                to_height: first.to_height,
                runs: count(group.len()),
                errors: count(group.len() - good.len()),
                mismatches: count(
                    group
                        .iter()
                        .filter(|r| r.verification_status == Verification::DiffersFromReference)
                        .count(),
                ),
                wall_ms_median: wall_median,
                wall_ms_min: min,
                wall_ms_max: max,
                wall_spread_percent: match (wall_median, min, max) {
                    (Some(median), Some(min), Some(max)) if median > 0.0 => {
                        Some((max - min) / median * 100.0)
                    }
                    _ => None,
                },
                cpu_ms_median: median(&cpu),
                downloaded_bytes_median: median(&bytes),
                requests_median: median(&requests),
                blocks: good.first().map(|r| r.blocks),
                txs_seen: good.first().map(|r| r.txs_seen),
                txs_scanned: good.first().map(|r| r.txs_scanned),
                detected_count: good.first().map(|r| r.detected_count),
            })
        })
        .collect()
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

fn opt(value: Option<f64>, digits: usize) -> String {
    value.map_or_else(String::new, |v| format!("{v:.digits$}"))
}

pub fn to_csv(file: &BenchFile) -> String {
    let mut out = String::from(
        "run_id,order,backend,backend_id,mode,network,evidence_kind,from_height,to_height,\
         measured_from_height,repeat,wall_ms,connect_ms,cpu_ms,requests,downloaded_bytes,blocks,\
         txs_seen,txs_scanned,detected_count,server_preprocess_ms,verification_status,error,\
         cache_condition\n",
    );
    for r in &file.runs {
        let verification = serde_json::to_value(r.verification_status)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let fields = [
            csv_field(&r.run_id),
            r.order.to_string(),
            csv_field(&r.backend),
            csv_field(&r.backend_id),
            r.mode.as_str().to_string(),
            r.network.to_string(),
            csv_field(&r.evidence_kind),
            r.from_height.to_string(),
            r.to_height.to_string(),
            r.measured_from_height.to_string(),
            r.repeat.to_string(),
            format!("{:.3}", r.wall_ms),
            format!("{:.3}", r.connect_ms),
            opt(r.cpu_ms, 0),
            r.requests.to_string(),
            r.downloaded_bytes.to_string(),
            r.blocks.to_string(),
            r.txs_seen.to_string(),
            r.txs_scanned.to_string(),
            r.detected_count.to_string(),
            opt(r.server_preprocess_ms, 3),
            verification,
            csv_field(r.error.as_deref().unwrap_or("")),
            csv_field(&r.cache_condition),
        ];
        let _ = writeln!(out, "{}", fields.join(","));
    }
    out
}

/// Makes text safe inside a Markdown table cell.
fn cell(text: &str) -> String {
    single_line(text).replace('|', "\\|")
}

pub fn to_markdown(file: &BenchFile) -> String {
    let mut out = String::new();
    let mut line = |text: String| {
        let _ = writeln!(out, "{text}");
    };
    let env = &file.environment;
    line("# Silent Sync Lab — benchmark results".to_string());
    line(String::new());
    line("EXPERIMENTAL — NOT FOR MAINNET OR REAL FUNDS. Generated by `silent-sync export`; do not edit by hand.".to_string());
    line(String::new());
    line("## Environment".to_string());
    line(String::new());
    line("| item | value |".to_string());
    line("|---|---|".to_string());
    line(format!("| tool version | {} |", cell(&env.tool_version)));
    line(format!(
        "| scanner version | {} |",
        cell(&env.scanner_version)
    ));
    line(format!("| compiler | {} |", cell(&env.rustc)));
    line(format!("| target | {} |", cell(&env.target)));
    line(format!("| build profile | {} |", cell(&env.build_profile)));
    line(format!(
        "| OS / architecture | {} / {} |",
        cell(&env.os),
        cell(&env.arch)
    ));
    line(format!(
        "| logical CPUs | {} |",
        env.logical_cpus
            .map_or("unknown".to_string(), |n| n.to_string())
    ));
    for note in &env.notes {
        line(format!("| note | {} |", cell(note)));
    }
    line(String::new());

    line("## What the numbers mean".to_string());
    line(String::new());
    for text in [
        "Every number is measured in the client process: wall-clock time, its CPU time and the response bytes it received. Time and storage a server spent beforehand (a node validating and storing blocks, an index server computing tweak data) are **not included** and are not estimated; `server_preprocess_ms` is empty for that reason.",
        "`wall` includes opening the source (`connect`). CPU time is read from the operating system in clock ticks (typically 10 ms), so short runs show 0 or 10.",
        "All services ran on the same machine over loopback. These results say nothing about performance over the internet, and results from the offline fixture say nothing about a node's or a server's response times.",
        "Modes: `scan` = stateless scan of the range; `sync_cold` = new store, whole range; `sync_resume` = store already at the middle, second half measured; `sync_noop` = store already at the end; `restore` = store deleted and rebuilt from the same birthday, then compared with what it held before.",
        "`same result` counts runs whose detected outputs equal those of the first successful run in the file. It shows that the runs agree with each other, not that the result is complete.",
    ] {
        line(format!("- {text}"));
    }
    line(String::new());

    line("## Summary (median of the successful runs)".to_string());
    line(String::new());
    line("| range | blocks | backend | mode | runs | errors | wall median ms | min | max | spread % | CPU median ms | downloaded bytes | requests | tx seen | tx scanned | detected | same result |".to_string());
    line("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|".to_string());
    for s in summarize(&file.runs) {
        let n = |v: Option<u64>| v.map_or_else(|| "—".to_string(), |v| v.to_string());
        let f = |v: Option<f64>, digits: usize| {
            v.map_or_else(|| "—".to_string(), |v| format!("{v:.digits$}"))
        };
        line(format!(
            "| {}..={} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            s.from_height,
            s.to_height,
            s.to_height - s.from_height + 1,
            cell(&s.backend),
            s.mode.as_str(),
            s.runs,
            s.errors,
            f(s.wall_ms_median, 2),
            f(s.wall_ms_min, 2),
            f(s.wall_ms_max, 2),
            f(s.wall_spread_percent, 0),
            f(s.cpu_ms_median, 0),
            f(s.downloaded_bytes_median, 0),
            f(s.requests_median, 0),
            n(s.txs_seen),
            n(s.txs_scanned),
            n(s.detected_count),
            if s.mismatches == 0 && s.errors == 0 {
                "yes".to_string()
            } else {
                format!("NO ({} differ, {} failed)", s.mismatches, s.errors)
            },
        ));
    }
    line(String::new());

    line("## All runs, in execution order".to_string());
    line(String::new());
    line("| # | backend | mode | range | repeat | wall ms | connect ms | CPU ms | bytes | requests | blocks | detected | result | error |".to_string());
    line("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|".to_string());
    for r in &file.runs {
        let verification = match r.verification_status {
            Verification::Reference => "reference",
            Verification::SameAsReference => "same",
            Verification::DiffersFromReference => "DIFFERS",
            Verification::NotRun => "not run",
        };
        line(format!(
            "| {} | {} | {} | {}..={} | {} | {:.2} | {:.2} | {} | {} | {} | {} | {} | {} | {} |",
            r.order,
            cell(&r.backend),
            r.mode.as_str(),
            r.from_height,
            r.to_height,
            r.repeat,
            r.wall_ms,
            r.connect_ms,
            r.cpu_ms.map_or("—".to_string(), |v| format!("{v:.0}")),
            r.downloaded_bytes,
            r.requests,
            r.blocks,
            r.detected_count,
            verification,
            cell(r.error.as_deref().unwrap_or("")),
        ));
    }
    line(String::new());

    line("## Measured ranges".to_string());
    line(String::new());
    line("| range | network | first block hash | last block hash |".to_string());
    line("|---|---|---|---|".to_string());
    let mut seen = BTreeSet::new();
    for r in file.runs.iter().filter(|r| r.error.is_none()) {
        if seen.insert((
            r.from_height,
            r.to_height,
            r.first_block_hash.clone(),
            r.last_block_hash.clone(),
        )) {
            line(format!(
                "| {}..={} | {} | `{}` | `{}` |",
                r.from_height,
                r.to_height,
                r.network,
                cell(&r.first_block_hash),
                cell(&r.last_block_hash)
            ));
        }
    }
    out
}

/// Merges result files (e.g. one per range) into one, keeping every run and
/// renumbering the execution order file by file.
pub fn merge(files: Vec<BenchFile>) -> Result<BenchFile, String> {
    let mut merged: Option<BenchFile> = None;
    for file in files {
        if file.format != FORMAT {
            return Err(format!(
                "unsupported results format `{}` (expected `{FORMAT}`)",
                single_line(&file.format)
            ));
        }
        match merged.as_mut() {
            None => merged = Some(file),
            Some(all) => {
                let mut environment = file.environment;
                // Notes describe a run; the rest must be the same build.
                let notes = std::mem::take(&mut environment.notes);
                let mut base = all.environment.clone();
                base.notes.clear();
                if environment != base {
                    return Err(
                        "the results files were produced by different builds or machines"
                            .to_string(),
                    );
                }
                for note in notes {
                    if !all.environment.notes.contains(&note) {
                        all.environment.notes.push(note);
                    }
                }
                let offset = all.runs.iter().map(|r| r.order).max().unwrap_or(0);
                all.runs.extend(file.runs.into_iter().map(|mut r| {
                    r.order += offset;
                    r
                }));
            }
        }
    }
    merged.ok_or_else(|| "no results file given".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_of_odd_even_and_empty_inputs() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0]), Some(3.0));
        assert_eq!(median(&[5.0, 1.0, 3.0]), Some(3.0));
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), Some(2.5));
        assert_eq!(median(&[f64::NAN, 2.0]), Some(2.0));
    }

    #[test]
    fn csv_fields_are_quoted_when_needed() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
    }

    #[test]
    fn cpu_clock_reads_this_process() {
        let clock = CpuClock::new();
        if cfg!(target_os = "linux") {
            let before = clock.now_ms().expect("CPU time is available on Linux");
            let mut x = 0u64;
            for i in 0..200_000_000u64 {
                x = x.wrapping_add(std::hint::black_box(i));
            }
            std::hint::black_box(x);
            let after = clock.now_ms().expect("CPU time is available on Linux");
            assert!(after >= before);
        }
    }

    #[test]
    fn modes_round_trip_through_their_names() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
            assert_eq!(
                serde_json::to_value(mode).ok(),
                Some(serde_json::Value::String(mode.as_str().to_string()))
            );
        }
        assert_eq!(Mode::parse("warp"), None);
    }
}
