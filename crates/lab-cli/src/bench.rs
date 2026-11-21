//! Measurements for `silent-sync bench` and their export.
//!
//! What is measured is this process only: the time and CPU the *client* spends
//! and the bytes it downloads. Work a server did beforehand (a node validating
//! blocks, an index server computing tweaks) is not in these numbers and is
//! reported as "not measured", never as zero.
//!
//! A results file holds no personal data: no transaction ids, no outputs, no
//! wallet identifier, no file paths, no URLs. It holds block hashes of the
//! measured range, which are public chain data. A failed run is recorded with
//! the *class* of its error only — error text can quote a transaction id, a
//! path or a URL, so it goes to stderr and never into the file. The one
//! free-form field is the notes the user passes on the command line.

use serde::{Deserialize, Serialize};
use silent_sync::{
    bitcoin::OutPoint,
    engine::{scan_range, ScanError},
    models::DetectedOutput,
    network::{ChainIdentity, Network},
    protocol::ReceiveScanner,
    source::{ScanSource, SourceError, SourceStats},
    store::{StoreError, SyncStore, SCANNER_VERSION},
    sync::{new_store_meta, sync, SyncError, SyncOptions},
};
use silent_sync_sqlite::SqliteStore;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    path::{Path, PathBuf},
    time::Instant,
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
    pub network: Network,
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
    /// Requests sent and response bytes received during the measured part
    /// (connecting to the source included).
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
    /// Class of the failure, for a failed run. Never the error text itself.
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
        Some(cpu_ticks(&stat)? * 1_000.0 / per_second)
    }
}

/// utime + stime, in clock ticks, from the contents of `/proc/<pid>/stat`.
/// The second field (the command name, in parentheses) may itself contain
/// spaces and parentheses; everything after the *last* closing parenthesis is
/// space separated, and utime and stime are the 14th and 15th fields overall.
fn cpu_ticks(stat: &str) -> Option<f64> {
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace().skip(11);
    let utime: f64 = fields.next()?.parse().ok()?;
    let stime: f64 = fields.next()?.parse().ok()?;
    Some(utime + stime)
}

impl Default for CpuClock {
    fn default() -> Self {
        Self::new()
    }
}

/// Opens a fresh connection to one backend.
pub type OpenSource<'a> = &'a dyn Fn() -> Result<Box<dyn ScanSource>, String>;

pub struct BenchPlan<'a> {
    pub network: Network,
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

/// Why a run failed: a fixed class for the results file, and the full text
/// for the terminal only.
#[derive(Debug)]
pub struct RunError {
    pub class: &'static str,
    pub detail: String,
}

fn source_class(e: &SourceError) -> &'static str {
    match e {
        SourceError::InvalidRange { .. } => "source: invalid range",
        SourceError::RangeUnavailable { .. } => "source: range unavailable",
        SourceError::InvalidData(_) => "source: invalid data",
        SourceError::Unavailable(_) => "source: unavailable",
        SourceError::Network(_) => "source: network refused",
        _ => "source: other error",
    }
}

fn scan_class(e: &ScanError) -> &'static str {
    match e {
        ScanError::ChainMismatch { .. } => "scan: chain mismatch",
        ScanError::InvalidRange { .. } => "scan: invalid range",
        ScanError::Source(e) => source_class(e),
        ScanError::InconsistentBatch { .. } => "scan: inconsistent batch",
        ScanError::Transaction { .. } => "scan: transaction error",
        _ => "scan: other error",
    }
}

fn store_class(e: &StoreError) -> &'static str {
    match e {
        StoreError::NotInitialized => "store: not initialized",
        StoreError::AlreadyInitialized => "store: already initialized",
        StoreError::Invariant(_) => "store: invariant violated",
        StoreError::Backend(_) => "store: backend error",
        _ => "store: other error",
    }
}

fn sync_class(e: &SyncError) -> &'static str {
    match e {
        SyncError::Store(e) => store_class(e),
        SyncError::Scan(e) => scan_class(e),
        SyncError::BindingMismatch { .. } => "sync: binding mismatch",
        SyncError::SourceBehindCheckpoint { .. } => "sync: source behind checkpoint",
        SyncError::CompetingChainShorter { .. } => "sync: competing chain shorter than the store",
        SyncError::TargetAboveTip { .. } => "sync: target above tip",
        SyncError::ChainMovedDuringSync { .. } => "sync: chain moved during sync",
        SyncError::InvalidChunkSize => "sync: invalid chunk size",
        SyncError::ReorgTooDeep { .. } => "sync: reorganization deeper than the limit",
        SyncError::Interrupted { cause, .. } => sync_class(cause),
        _ => "sync: other error",
    }
}

impl From<SourceError> for RunError {
    fn from(e: SourceError) -> Self {
        Self {
            class: source_class(&e),
            detail: e.to_string(),
        }
    }
}

impl From<ScanError> for RunError {
    fn from(e: ScanError) -> Self {
        Self {
            class: scan_class(&e),
            detail: e.to_string(),
        }
    }
}

impl From<StoreError> for RunError {
    fn from(e: StoreError) -> Self {
        Self {
            class: store_class(&e),
            detail: e.to_string(),
        }
    }
}

impl From<SyncError> for RunError {
    fn from(e: SyncError) -> Self {
        Self {
            class: sync_class(&e),
            detail: e.to_string(),
        }
    }
}

/// Opens the source; a failure is recorded as such, with its text kept out
/// of the results.
fn opened(open: OpenSource<'_>) -> Result<Box<dyn ScanSource>, RunError> {
    open().map_err(|detail| RunError {
        class: "source could not be opened",
        detail,
    })
}

struct Runner<'a> {
    cpu: &'a CpuClock,
    network: Network,
    scanner: &'a ReceiveScanner,
    from: u32,
    to: u32,
}

impl Runner<'_> {
    fn chain(&self) -> ChainIdentity {
        ChainIdentity::for_network(self.network)
    }

    fn options(&self, target: u32) -> SyncOptions {
        SyncOptions::default().with_target_height(Some(target))
    }

    /// Creates a store bound to `source` with the range start as birthday.
    fn new_store(&self, source: &dyn ScanSource, path: &Path) -> Result<SqliteStore, RunError> {
        let mut store = SqliteStore::create(path)?;
        store.initialize(&new_store_meta(
            source,
            self.scanner,
            self.from,
            "benchmark: first height of the measured range",
        ))?;
        Ok(store)
    }

    /// Not measured: brings a new store at `path` up to `target`.
    fn prepare(&self, open: OpenSource<'_>, path: &Path, target: u32) -> Result<(), RunError> {
        let source = opened(open)?;
        let mut store = self.new_store(source.as_ref(), path)?;
        sync(
            source.as_ref(),
            self.scanner,
            &mut store,
            &self.options(target),
        )?;
        Ok(())
    }

    fn run(
        &self,
        mode: Mode,
        open: OpenSource<'_>,
        store: &TempStore,
    ) -> Result<Measured, RunError> {
        let middle = self.from + (self.to - self.from) / 2;
        let mut original = None;
        match mode {
            Mode::Scan | Mode::SyncCold => {}
            Mode::SyncResume => self.prepare(open, &store.path, middle)?,
            Mode::SyncNoop => self.prepare(open, &store.path, self.to)?,
            Mode::Restore => {
                self.prepare(open, &store.path, self.to)?;
                let held = SqliteStore::open(&store.path).and_then(|s| s.detections())?;
                original = Some(keys(&held));
                store.remove();
            }
        }

        // ---- measured part starts with opening the source ----
        let cpu_before = self.cpu.now_ms();
        let started = Instant::now();
        let source = opened(open)?;
        let connect_ms = ms(started);
        let source = source.as_ref();
        let capabilities = source.capabilities();
        let evidence_kind = serde_json::to_value(capabilities.evidence_kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();

        let (blocks, txs_seen, txs_scanned, skipped, detections, measured_from) = match mode {
            Mode::Scan => {
                let report = scan_range(source, self.scanner, self.chain(), self.from, self.to)?;
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
                    _ => SqliteStore::open(&store.path)?,
                };
                let report = sync(source, self.scanner, &mut db, &self.options(self.to))?;
                let reached = report.checkpoint.map(|c| c.height);
                if reached != Some(self.to) {
                    return Err(RunError {
                        class: "sync stopped before the end of the range",
                        detail: format!(
                            "sync stopped at {reached:?} instead of {} ({:?})",
                            self.to, report.stopped_because
                        ),
                    });
                }
                let measured_from = report
                    .resumed_from
                    .map_or(self.from, |c| c.height.saturating_add(1));
                (
                    report.blocks_scanned,
                    report.txs_seen,
                    report.txs_scanned,
                    report.txs_skipped,
                    keys(&db.detections()?),
                    measured_from,
                )
            }
        };
        // The source was opened inside the measured part, so its counters are
        // exactly what the measured part transferred (connecting included).
        let stats = source.stats();
        let wall_ms = ms(started);
        let cpu_ms = match (cpu_before, self.cpu.now_ms()) {
            (Some(before), Some(after)) => Some(after - before),
            _ => None,
        };
        // ---- measured part ends ----

        let (first, last) = (source.block_ref(self.from)?, source.block_ref(self.to)?);
        Ok(Measured {
            backend: source.backend_id().to_string(),
            evidence_kind,
            first_block_hash: first.hash.to_string(),
            last_block_hash: last.hash.to_string(),
            measured_from_height: measured_from,
            wall_ms,
            connect_ms,
            cpu_ms,
            stats,
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
    progress: &mut dyn FnMut(&BenchRecord, Option<&str>),
) -> Vec<BenchRecord> {
    let cpu = CpuClock::new();
    let runner = Runner {
        cpu: &cpu,
        network: plan.network,
        scanner,
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
                let mut detail = None;
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
                    Err(e) => {
                        record.error = Some(e.class.to_string());
                        detail = Some(single_line(&e.detail));
                    }
                }
                progress(&record, detail.as_deref());
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
        "`wall`, the request count and the downloaded bytes all include opening the source (`connect`). CPU time is read from the operating system in clock ticks (typically 10 ms), so short runs show 0 or 10.",
        "A failed run is listed with the class of its error only; the error text is not recorded.",
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

    derived_tables(&summarize(&file.runs), &mut line);

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

fn ratio(a: Option<f64>, b: Option<f64>) -> String {
    match (a, b) {
        (Some(a), Some(b)) if b > 0.0 => format!("{:.2}", a / b),
        _ => "—".to_string(),
    }
}

/// Figures derived from the medians of the summary, so that a report can
/// quote them without anyone computing them by hand.
fn derived_tables(summaries: &[Summary], line: &mut dyn FnMut(String)) {
    let find = |from: u32, to: u32, backend: &str, mode: Mode| {
        summaries.iter().find(|s| {
            s.from_height == from && s.to_height == to && s.backend == backend && s.mode == mode
        })
    };
    let per = |value: Option<f64>, blocks: u32, digits: usize| {
        value.map_or_else(
            || "—".to_string(),
            |v| format!("{:.digits$}", v / f64::from(blocks)),
        )
    };

    line("## Derived figures (from the medians above)".to_string());
    line(String::new());
    line("### Per block of the range".to_string());
    line(String::new());
    line("| range | blocks | backend | mode | wall ms / block | CPU ms / block | bytes / block | requests / block |".to_string());
    line("|---|---|---|---|---|---|---|---|".to_string());
    for s in summaries
        .iter()
        .filter(|s| matches!(s.mode, Mode::Scan | Mode::SyncCold))
    {
        let blocks = s.to_height - s.from_height + 1;
        line(format!(
            "| {}..={} | {} | {} | {} | {} | {} | {} | {} |",
            s.from_height,
            s.to_height,
            blocks,
            cell(&s.backend),
            s.mode.as_str(),
            per(s.wall_ms_median, blocks, 3),
            per(s.cpu_ms_median, blocks, 3),
            per(s.downloaded_bytes_median, blocks, 1),
            per(s.requests_median, blocks, 2),
        ));
    }
    line(String::new());

    line(
        "### Backends on the same range (each relative to the first backend listed for that range)"
            .to_string(),
    );
    line(String::new());
    line("| range | mode | backend | relative to | wall | CPU | bytes | requests |".to_string());
    line("|---|---|---|---|---|---|---|---|".to_string());
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for s in summaries {
        if !ranges.contains(&(s.from_height, s.to_height)) {
            ranges.push((s.from_height, s.to_height));
        }
    }
    for &(from, to) in &ranges {
        let mut backends: Vec<&str> = Vec::new();
        for s in summaries
            .iter()
            .filter(|s| (s.from_height, s.to_height) == (from, to))
        {
            if !backends.contains(&s.backend.as_str()) {
                backends.push(&s.backend);
            }
        }
        let Some((base, others)) = backends.split_first() else {
            continue;
        };
        for mode in Mode::ALL {
            for other in others {
                if let (Some(a), Some(b)) =
                    (find(from, to, other, mode), find(from, to, base, mode))
                {
                    line(format!(
                        "| {from}..={to} | {} | {} | {} | {} | {} | {} | {} |",
                        mode.as_str(),
                        cell(other),
                        cell(base),
                        ratio(a.wall_ms_median, b.wall_ms_median),
                        ratio(a.cpu_ms_median, b.cpu_ms_median),
                        ratio(a.downloaded_bytes_median, b.downloaded_bytes_median),
                        ratio(a.requests_median, b.requests_median),
                    ));
                }
            }
        }
    }
    line(String::new());

    line("### Modes of one backend".to_string());
    line(String::new());
    line("| range | backend | sync_cold / scan | sync_cold − scan (ms) | sync_resume / sync_cold | restore / sync_cold | sync_noop wall ms | sync_noop bytes |".to_string());
    line("|---|---|---|---|---|---|---|---|".to_string());
    for cold in summaries.iter().filter(|s| s.mode == Mode::SyncCold) {
        let other = |mode| find(cold.from_height, cold.to_height, &cold.backend, mode);
        let wall = |mode| other(mode).and_then(|s| s.wall_ms_median);
        let number = |v: Option<f64>, digits: usize| {
            v.map_or_else(|| "—".to_string(), |v| format!("{v:.digits$}"))
        };
        line(format!(
            "| {}..={} | {} | {} | {} | {} | {} | {} | {} |",
            cold.from_height,
            cold.to_height,
            cell(&cold.backend),
            ratio(cold.wall_ms_median, wall(Mode::Scan)),
            number(
                cold.wall_ms_median
                    .zip(wall(Mode::Scan))
                    .map(|(cold, scan)| cold - scan),
                2
            ),
            ratio(wall(Mode::SyncResume), cold.wall_ms_median),
            ratio(wall(Mode::Restore), cold.wall_ms_median),
            number(wall(Mode::SyncNoop), 2),
            number(
                other(Mode::SyncNoop).and_then(|s| s.downloaded_bytes_median),
                0
            ),
        ));
    }
    line(String::new());
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
    fn cpu_ticks_are_the_14th_and_15th_fields_whatever_the_command_name() {
        let tail = "S 1 2 3 4 5 6 7 8 9 10 111 222 13 14 15";
        for comm in ["(silent-sync)", "(a b c)", "(evil) R 9 9 9 9)", "((()))"] {
            assert_eq!(
                cpu_ticks(&format!("4242 {comm} {tail}")),
                Some(333.0),
                "{comm}"
            );
        }
        assert_eq!(cpu_ticks("4242 (x) S 1 2 3"), None);
        assert_eq!(cpu_ticks("no parenthesis at all"), None);
    }

    fn record(backend: &str, mode: Mode, wall_ms: f64) -> BenchRecord {
        BenchRecord {
            run_id: String::new(),
            order: 1,
            backend: backend.to_string(),
            backend_id: backend.to_string(),
            mode,
            network: Network::Regtest,
            evidence_kind: "full_block_with_prevouts".to_string(),
            from_height: 1,
            to_height: 10,
            first_block_hash: "aa".to_string(),
            last_block_hash: "bb".to_string(),
            measured_from_height: 1,
            repeat: 1,
            cache_condition: String::new(),
            wall_ms,
            connect_ms: 1.0,
            cpu_ms: Some(10.0),
            requests: 20,
            downloaded_bytes: 1_000,
            blocks: 10,
            txs_seen: 50,
            txs_scanned: 20,
            txs_skipped: BTreeMap::new(),
            detected_count: 1,
            labels_scanned: vec![0],
            server_preprocess_ms: None,
            verification_status: Verification::SameAsReference,
            error: None,
        }
    }

    fn file(runs: Vec<BenchRecord>) -> BenchFile {
        BenchFile {
            format: FORMAT.to_string(),
            environment: Environment::capture(vec!["a note".to_string()]),
            runs,
        }
    }

    #[test]
    fn failed_and_differing_runs_are_not_counted_as_good() {
        let mut failed = record("core", Mode::Scan, 9_999.0);
        failed.error = Some("source: unavailable".to_string());
        failed.verification_status = Verification::NotRun;
        let mut differing = record("core", Mode::Scan, 30.0);
        differing.verification_status = Verification::DiffersFromReference;
        let runs = vec![
            record("core", Mode::Scan, 10.0),
            failed,
            record("core", Mode::Scan, 20.0),
            differing,
        ];
        let summary = summarize(&runs);
        assert_eq!(summary.len(), 1);
        let s = &summary[0];
        assert_eq!((s.runs, s.errors, s.mismatches), (4, 1, 1));
        // The failed run's numbers take no part in the statistics.
        assert_eq!(s.wall_ms_median, Some(20.0));
        assert_eq!((s.wall_ms_min, s.wall_ms_max), (Some(10.0), Some(30.0)));
        assert_eq!(s.wall_spread_percent, Some(100.0));

        let markdown = to_markdown(&file(runs.clone()));
        assert!(
            markdown.contains("| NO (1 differ, 1 failed) |"),
            "{markdown}"
        );
        assert!(
            markdown.contains("| DIFFERS |")
                && markdown.contains("| not run | source: unavailable |")
        );
        let all_good = to_markdown(&file(vec![record("core", Mode::Scan, 10.0)]));
        assert!(all_good.contains("| yes |") && !all_good.contains("NO ("));
    }

    #[test]
    fn derived_figures_follow_from_the_medians() {
        let mut oracle = record("oracle-v2", Mode::Scan, 30.0);
        oracle.cpu_ms = Some(5.0);
        oracle.downloaded_bytes = 500;
        oracle.requests = 10;
        let runs = vec![
            record("core", Mode::Scan, 20.0),
            oracle,
            record("core", Mode::SyncCold, 50.0),
            record("core", Mode::SyncResume, 25.0),
            record("core", Mode::Restore, 55.0),
            record("core", Mode::SyncNoop, 2.0),
        ];
        let markdown = to_markdown(&file(runs));
        for needle in [
            // per block: 20 ms, 10 ms CPU, 1000 bytes, 20 requests over 10 blocks
            "| 1..=10 | 10 | core | scan | 2.000 | 1.000 | 100.0 | 2.00 |",
            // oracle relative to core: 30/20, 5/10, 500/1000, 10/20
            "| 1..=10 | scan | oracle-v2 | core | 1.50 | 0.50 | 0.50 | 0.50 |",
            // modes: 50/20, 50-20, 25/50, 55/50, noop 2 ms and 1000 bytes
            "| 1..=10 | core | 2.50 | 30.00 | 0.50 | 1.10 | 2.00 | 1000 |",
        ] {
            assert!(
                markdown.contains(needle),
                "missing `{needle}` in:\n{markdown}"
            );
        }
    }

    #[test]
    fn results_of_different_builds_are_not_merged() {
        let a = file(vec![record("core", Mode::Scan, 10.0)]);
        let mut same_build = a.clone();
        same_build.environment.notes = vec!["another note".to_string()];
        let merged = merge(vec![a.clone(), same_build]).expect("same build");
        assert_eq!(merged.runs.len(), 2);
        assert_eq!(
            merged.runs.iter().map(|r| r.order).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(merged.environment.notes, vec!["a note", "another note"]);

        let mut other_build = a.clone();
        other_build.environment.rustc = "rustc 0.0.0".to_string();
        assert!(merge(vec![a.clone(), other_build])
            .is_err_and(|e| e.contains("different builds or machines")));
        let mut other_format = a.clone();
        other_format.format = "something-else".to_string();
        assert!(merge(vec![other_format]).is_err_and(|e| e.contains("unsupported results format")));
        assert!(merge(Vec::new()).is_err());
    }

    /// A source that changes between the preparation and the measured part:
    /// the restored store no longer equals the one it replaced, and the run
    /// must say so instead of being counted as a good run.
    #[test]
    fn a_restore_that_differs_from_the_deleted_store_is_a_failed_run() {
        use lab_testkit::fixture_gen;
        use silent_sync::source::fixture::FixtureSource;
        use std::cell::Cell;

        let keys = fixture_gen::lab_receiver_keys();
        let scanner = ReceiveScanner::new(&keys, &[fixture_gen::LABEL_USED]).expect("scanner");
        let calls = Cell::new(0u32);
        // First connection (preparation): the main chain. Afterwards: the fork.
        let flipping = || -> Result<Box<dyn ScanSource>, String> {
            let fixture = if calls.replace(calls.get() + 1) == 0 {
                fixture_gen::generate()
            } else {
                fixture_gen::generate_fork()
            };
            Ok(Box::new(
                FixtureSource::from_file(fixture).map_err(|e| e.to_string())?,
            ))
        };
        fn plan<'a>(modes: Vec<Mode>, open: OpenSource<'a>) -> BenchPlan<'a> {
            BenchPlan {
                network: Network::Regtest,
                backends: vec![("fixture".to_string(), open)],
                modes,
                from: 1,
                to: 8,
                repeat: 1,
                work_dir: std::env::temp_dir(),
            }
        }
        let mut details = Vec::new();
        let runs = run_plan(
            &plan(vec![Mode::Restore], &flipping),
            &scanner,
            &mut |_, detail| details.push(detail.map(str::to_string)),
        );
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].error.as_deref(),
            Some("the restored store differs from the store it replaced")
        );
        assert_eq!(
            runs[0].verification_status,
            Verification::DiffersFromReference
        );

        // A source that cannot be opened: the class is recorded, the text
        // (here with a path in it) only reaches the progress callback.
        let broken = || -> Result<Box<dyn ScanSource>, String> {
            Err("cannot read /home/someone/secret/fixture.json".to_string())
        };
        let mut details = Vec::new();
        let runs = run_plan(
            &plan(vec![Mode::Scan, Mode::SyncCold], &broken),
            &scanner,
            &mut |_, detail| details.push(detail.map(str::to_string)),
        );
        assert_eq!(runs.len(), 2);
        for run in &runs {
            assert_eq!(run.error.as_deref(), Some("source could not be opened"));
            assert_eq!(run.verification_status, Verification::NotRun);
        }
        assert!(details.iter().all(|d| d
            .as_deref()
            .is_some_and(|d| d.contains("/home/someone/secret"))));
        let json = serde_json::to_string(&runs).expect("json");
        assert!(!json.contains("someone") && !json.contains("secret"));
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
