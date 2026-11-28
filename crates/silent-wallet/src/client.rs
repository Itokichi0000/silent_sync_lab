//! The one-stop interface: a silent payments wallet behind a handful of
//! calls.
//!
//! ```no_run
//! use silent_wallet::client::{ClientConfig, SeedPhrase, WalletClient, WordCount};
//! use silent_sync::Network;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Once: make a seed phrase and have the user write it down.
//! let phrase = SeedPhrase::generate(WordCount::Twelve)?;
//! // (show `phrase.reveal()` to the user; nothing here stores it)
//!
//! // On every start: connect with the phrase.
//! let config = ClientConfig::new(Network::Signet, "wallet.sqlite3", "http://127.0.0.1:38332")
//!     .with_rpc_cookie_file("/path/to/signet/.cookie");
//! let mut wallet = WalletClient::connect(config, &phrase, "")?;
//!
//! // Receive.
//! println!("pay to {}", wallet.receive_address(None)?);
//! wallet.sync()?;
//! println!("{} sat unspent", wallet.holdings()?.unspent_sats());
//!
//! // Send: prepare, show, then send.
//! let prepared = wallet.prepare_send("tsp1q…", 10_000, 2)?;
//! println!("{prepared}"); // the user decides on this
//! let txid = wallet.send(prepared)?;
//! # let _ = txid; Ok(()) }
//! ```
//!
//! What this is, in one paragraph: the data source is **your own Bitcoin
//! Core node on this machine** (REST for reading, JSON-RPC with the cookie
//! file for the one call that sends). Receiving is through silent payment
//! addresses; sending is to silent payment addresses and to ordinary
//! on-chain addresses. The store on disk holds detected outputs and
//! checkpoints, never keys; the phrase is asked for on every start.
//!
//! What it is not: it does not receive at ordinary on-chain addresses, has
//! no Lightning, does not estimate fees, and does not keep the phrase for
//! you. The code is unaudited.
//!
//! ## Sending is two calls on purpose
//!
//! [`WalletClient::prepare_send`] builds and signs but sends nothing; the
//! [`PreparedSend`] it returns says exactly what would happen (inputs,
//! outputs, change, fee). An application must show that to its user and
//! call [`WalletClient::send`] only on the user's decision. There is no
//! call that does both.
//!
//! If the node's answer to `send` is lost, the result is
//! [`ClientError::OutcomeUnknown`]: the transaction may or may not be on its
//! way. It stays on record as pending so that its inputs are not used
//! again; never report that case as "not sent".

use crate::{
    holdings::{holdings, Holdings},
    keys::{KeyError, Wallet, WalletError, WalletKeys},
    pending::{PendingError, PendingSpends, PendingTransaction, Settlement},
    rpc::{CoreRpc, RpcError},
    send::{build_and_sign, BuiltTransaction, Destination, Payment, SendError, SendLimits},
};
use bdk_sp::bitcoin::{OutPoint, Txid};
use silent_sync::{
    new_store_meta,
    source::{CoreRestSource, SourceError},
    store::StoreError,
    sync, BlockRef, DetectedOutput, Network, ScanSource, SilentPaymentAddress, SpentOutput,
    StoreMeta, SyncError, SyncOptions, SyncReport, SyncStore,
};
use silent_sync_sqlite::SqliteStore;
use std::{
    fmt,
    path::{Path, PathBuf},
};

/// The most one mainnet transaction pays unless the configuration says
/// otherwise ([`ClientConfig::with_max_amount_sats`]).
pub const MAINNET_DEFAULT_MAX_AMOUNT_SATS: u64 = 100_000;

/// Why a call on the client failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    #[error("not a valid seed phrase (BIP39, English word list)")]
    /// The text is not a BIP39 phrase. The text is never included.
    InvalidSeedPhrase,
    #[error("the operating system supplied no random data")]
    /// No entropy for a new phrase.
    NoEntropy,
    #[error(transparent)]
    /// The keys could not be derived.
    Key(#[from] KeyError),
    #[error(transparent)]
    /// The wallet could not be made (mainnet without the feature, labels).
    Wallet(#[from] WalletError),
    #[error(transparent)]
    /// The node could not be read, or what it returned was refused.
    Source(#[from] SourceError),
    #[error(transparent)]
    /// The store could not be read or written, or refused the data.
    Store(#[from] StoreError),
    #[error("the node is on {node}, but the wallet was asked for on {asked}")]
    /// The node is on another network than the configuration names.
    NodeOnAnotherNetwork {
        /// The node's network.
        node: Network,
        /// The configured network.
        asked: Network,
    },
    #[error("the store is for {stored}, but the wallet was asked for on {asked}")]
    /// The store file belongs to another network.
    StoreOnAnotherNetwork {
        /// The store's network.
        stored: Network,
        /// The configured network.
        asked: Network,
    },
    #[error("the store was created with another seed phrase (or passphrase)")]
    /// The keys are not the ones the store is bound to.
    WrongSeed,
    #[error(
        "the store scans for labels {stored:?}, but the configuration asks for {asked:?}; labels \
         are fixed when the store is created"
    )]
    /// The configuration names labels the store does not scan for.
    LabelsDiffer {
        /// Labels of the store (without the change label).
        stored: Vec<u32>,
        /// Labels in the configuration.
        asked: Vec<u32>,
    },
    #[error("the store file has no binding; it was not created completely")]
    /// A store file without its binding.
    StoreNotInitialized,
    #[error("{0}")]
    /// Synchronisation failed. When it was interrupted after some blocks,
    /// the store is consistent and holds what was scanned so far.
    Sync(#[from] SyncError),
    #[error("the store is not at the node's tip; sync again")]
    /// The store could not be brought to the node's tip, so nothing is spent.
    NotCaughtUp,
    #[error(
        "{amount_sats} sat is more than the limit of {limit_sats} sat for one transaction on \
         {network}"
    )]
    /// The amount is above the configured limit; nothing was built.
    AmountAboveLimit {
        /// What was asked for.
        amount_sats: u64,
        /// The limit in force.
        limit_sats: u64,
        /// The wallet's network.
        network: Network,
    },
    #[error("{0}; nothing was sent")]
    /// The transaction could not be built or signed. Nothing was sent.
    Build(#[from] SendError),
    #[error("no cookie file of the node is configured, so nothing can be sent")]
    /// Sending needs [`ClientConfig::with_rpc_cookie_file`].
    NoRpc,
    #[error("{0}; nothing was sent")]
    /// The node refused, or was never asked. Nothing was sent.
    NotSent(RpcError),
    #[error(
        "{cause}; IT IS NOT KNOWN whether the node took transaction {txid}. It stays recorded as \
         pending so that its inputs are not used again"
    )]
    /// The node's answer was lost. The transaction may have been sent.
    OutcomeUnknown {
        /// The transaction in question.
        txid: Txid,
        /// What went wrong with the answer.
        cause: RpcError,
    },
    #[error(
        "the node already has transaction {txid}: it was sent before. It is recorded as pending"
    )]
    /// The node refused the transaction because it has it already (in its
    /// mempool or in a block). Nothing new was sent, and the transaction
    /// **is** out: it is on record as pending.
    AlreadyWithNode {
        /// The transaction.
        txid: Txid,
    },
    #[error(transparent)]
    /// The record of pending transactions could not be read or written.
    Pending(#[from] PendingError),
    #[error(
        "the wallet changed since this transaction was prepared (it was prepared at height \
         {}); prepare it again",
        .prepared_at.map_or_else(|| "nothing scanned".to_string(), |h| h.to_string())
    )]
    /// [`WalletClient::send`] was given a transaction prepared against
    /// another state of the store, or by a client for another wallet.
    Stale {
        /// Checkpoint height the transaction was prepared at.
        prepared_at: Option<u32>,
    },
    #[error(
        "{0} cannot be held back: it is not among the outputs detected through full blocks \
         that are not spent (it may be spent, being spent, only claimed by an index, or unknown)"
    )]
    /// [`WalletClient::hold_back`] was given an output it cannot set aside
    /// ([`Holdings::can_hold`]).
    CannotHold(OutPoint),
}

/// How many words a new seed phrase has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordCount {
    /// 12 words (128 bits of entropy).
    Twelve,
    /// 24 words (256 bits of entropy).
    TwentyFour,
}

/// A BIP39 seed phrase (English word list).
///
/// Whoever has the phrase has the wallet. This type does not implement
/// `Display` and its `Debug` output is redacted; the words leave it through
/// [`SeedPhrase::reveal`] only. Nothing in this crate stores a phrase.
#[derive(Clone)]
pub struct SeedPhrase(bip39::Mnemonic);

impl SeedPhrase {
    /// A new phrase from the operating system's random number source.
    pub fn generate(words: WordCount) -> Result<Self, ClientError> {
        let mut entropy = [0u8; 32];
        let entropy = match words {
            WordCount::Twelve => &mut entropy[..16],
            WordCount::TwentyFour => &mut entropy[..],
        };
        getrandom::getrandom(entropy).map_err(|_| ClientError::NoEntropy)?;
        Self::from_entropy(entropy)
    }

    /// The phrase for given entropy (16 or 32 bytes), for callers with their
    /// own source of randomness.
    pub fn from_entropy(entropy: &[u8]) -> Result<Self, ClientError> {
        if !matches!(entropy.len(), 16 | 32) {
            return Err(ClientError::InvalidSeedPhrase);
        }
        bip39::Mnemonic::from_entropy_in(bip39::Language::English, entropy)
            .map(Self)
            .map_err(|_| ClientError::InvalidSeedPhrase)
    }

    /// Reads a phrase a user typed: 12 or 24 English words with a valid
    /// checksum, in any case and with any spacing. The error never contains
    /// the text.
    pub fn parse(text: &str) -> Result<Self, ClientError> {
        // As typed: any spacing, any case. (The English list is lower case.)
        let typed = text
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ");
        let phrase = bip39::Mnemonic::parse_in(bip39::Language::English, typed)
            .map_err(|_| ClientError::InvalidSeedPhrase)?;
        if !matches!(phrase.word_count(), 12 | 24) {
            return Err(ClientError::InvalidSeedPhrase);
        }
        Ok(Self(phrase))
    }

    /// The words, separated by single spaces. **This is the secret**: show
    /// it to the user once, to be written down; do not log or store it.
    pub fn reveal(&self) -> String {
        self.0.to_string()
    }

    /// The wallet's keys for `network`. `passphrase` is the optional BIP39
    /// passphrase ("25th word"); the empty string means none.
    pub fn keys(&self, passphrase: &str, network: Network) -> Result<WalletKeys, ClientError> {
        let seed = self.0.to_seed(passphrase);
        Ok(WalletKeys::from_seed(&seed, network)?)
    }
}

impl fmt::Debug for SeedPhrase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SeedPhrase(<redacted>)")
    }
}

/// Where the wallet lives and how it reaches the node.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    network: Network,
    store_path: PathBuf,
    core_rest_url: String,
    rpc_url: Option<String>,
    rpc_cookie_file: Option<PathBuf>,
    birthday: Option<u32>,
    labels: Vec<u32>,
    min_confirmations: u32,
    limits: SendLimits,
    max_amount_sats: Option<u64>,
}

impl ClientConfig {
    /// A configuration for `network`, with the store at `store_path` (keep
    /// it in a directory only you can write) and your own Bitcoin Core node
    /// at `core_rest_url` (started with `-rest=1`; loopback only, e.g.
    /// `http://127.0.0.1:38332`).
    pub fn new(
        network: Network,
        store_path: impl Into<PathBuf>,
        core_rest_url: impl Into<String>,
    ) -> Self {
        Self {
            network,
            store_path: store_path.into(),
            core_rest_url: core_rest_url.into(),
            rpc_url: None,
            rpc_cookie_file: None,
            birthday: None,
            labels: Vec::new(),
            min_confirmations: 1,
            limits: SendLimits::default(),
            max_amount_sats: None,
        }
    }

    /// The node's cookie file (in its data directory). Needed to send; read
    /// when a transaction is sent, never stored or shown.
    pub fn with_rpc_cookie_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.rpc_cookie_file = Some(path.into());
        self
    }

    /// JSON-RPC URL of the node, if it is not the REST URL (Bitcoin Core
    /// serves both on one port).
    pub fn with_rpc_url(mut self, url: impl Into<String>) -> Self {
        self.rpc_url = Some(url.into());
        self
    }

    /// First block height to scan when the store is created. Payments
    /// confirmed below it are never found. Default: the node's tip at that
    /// moment (right for a new phrase; a restored one needs the height the
    /// wallet was first used at).
    pub fn with_birthday(mut self, height: u32) -> Self {
        self.birthday = Some(height);
        self
    }

    /// Labels to scan for besides the change label. Fixed when the store is
    /// created.
    pub fn with_labels(mut self, labels: &[u32]) -> Self {
        self.labels = labels.to_vec();
        self
    }

    /// Confirmations an output needs to count as unspent and to be spent.
    /// Default 1.
    pub fn with_min_confirmations(mut self, confirmations: u32) -> Self {
        self.min_confirmations = confirmations;
        self
    }

    /// Fee limits for sending.
    pub fn with_send_limits(mut self, limits: SendLimits) -> Self {
        self.limits = limits;
        self
    }

    /// The most one transaction may pay. Default: no limit on regtest and
    /// signet, [`MAINNET_DEFAULT_MAX_AMOUNT_SATS`] on mainnet.
    pub fn with_max_amount_sats(mut self, sats: u64) -> Self {
        self.max_amount_sats = Some(sats);
        self
    }

    fn amount_limit(&self) -> Option<u64> {
        self.max_amount_sats.or(self
            .network
            .is_mainnet()
            .then_some(MAINNET_DEFAULT_MAX_AMOUNT_SATS))
    }
}

/// What one [`WalletClient::sync`] did.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SyncSummary {
    /// The SDK's report: blocks scanned, new outputs, new spends, a
    /// reorganization if one was followed.
    pub report: SyncReport,
    /// Pending transactions whose state changed: confirmed, back to
    /// unconfirmed after a reorganization, or made impossible by another
    /// transaction.
    pub settled: Vec<(PendingTransaction, Settlement)>,
}

/// A signed transaction that has not been sent, with everything a user
/// needs to see before deciding. Its `Display` output is that summary.
#[derive(Debug, Clone)]
pub struct PreparedSend {
    built: BuiltTransaction,
    network: Network,
    /// The wallet (its public keys' identifier) the transaction spends from.
    wallet_id: String,
    /// The store's checkpoint when it was built: height and block hash.
    prepared_at: Option<BlockRef>,
}

impl PreparedSend {
    /// The transaction and its details: inputs, payments, change, fee.
    pub fn details(&self) -> &BuiltTransaction {
        &self.built
    }

    /// The transaction id.
    pub fn txid(&self) -> Txid {
        self.built.txid
    }

    /// The fee in satoshi.
    pub fn fee_sats(&self) -> u64 {
        self.built.fee_sats
    }
}

impl fmt::Display for PreparedSend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let built = &self.built;
        writeln!(f, "network     : {}", self.network)?;
        writeln!(f, "transaction : {}", built.txid)?;
        for payment in &built.payments {
            writeln!(
                f,
                "pays        : {} sat to {}",
                payment.amount_sats, payment.to
            )?;
        }
        match &built.change {
            Some(change) => writeln!(
                f,
                "change      : {} sat back to this wallet",
                change.amount_sats
            )?,
            None => writeln!(f, "change      : none")?,
        }
        writeln!(
            f,
            "fee         : {} sat ({} vbytes; {} sat/vB asked)",
            built.fee_sats, built.vsize, built.fee_rate_asked
        )?;
        for warning in built.warnings() {
            writeln!(f, "note        : {warning}")?;
        }
        let spent: u64 = built.inputs.iter().map(|i| i.value_sats).sum();
        write!(
            f,
            "spends      : {} outputs worth {spent} sat",
            built.inputs.len()
        )
    }
}

/// A silent payments wallet: keys in memory, a store on disk, your node as
/// the source of truth.
pub struct WalletClient {
    wallet: Wallet,
    source: CoreRestSource,
    store: SqliteStore,
    meta: StoreMeta,
    config: ClientConfig,
}

impl fmt::Debug for WalletClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletClient")
            .field("wallet", &self.wallet)
            .field("store", &self.config.store_path)
            .finish_non_exhaustive()
    }
}

fn pending_path(store: &Path) -> PathBuf {
    let mut name = store.as_os_str().to_os_string();
    name.push(".pending.json");
    name.into()
}

impl WalletClient {
    /// Opens the wallet, creating its store on first use.
    ///
    /// Fails — without creating or changing anything — if the node is on
    /// another network, if an existing store belongs to another network or
    /// to another phrase, or if the configuration names labels an existing
    /// store does not scan for.
    pub fn connect(
        config: ClientConfig,
        phrase: &SeedPhrase,
        passphrase: &str,
    ) -> Result<Self, ClientError> {
        let network = config.network;
        let keys = phrase.keys(passphrase, network)?;
        // Refuses mainnet here, before the node is contacted, in a build
        // without the `mainnet` feature.
        let wallet = Wallet::new(keys.clone(), network, &config.labels)?;
        let source = CoreRestSource::connect(&config.core_rest_url)?;
        if source.chain().network() != network {
            return Err(ClientError::NodeOnAnotherNetwork {
                node: source.chain().network(),
                asked: network,
            });
        }

        if config.store_path.exists() {
            let store = SqliteStore::open(&config.store_path)?;
            let meta = store.meta()?.ok_or(ClientError::StoreNotInitialized)?;
            if meta.chain.network() != network {
                return Err(ClientError::StoreOnAnotherNetwork {
                    stored: meta.chain.network(),
                    asked: network,
                });
            }
            let stored: Vec<u32> = meta.labels.iter().copied().filter(|&m| m != 0).collect();
            let mut asked = config.labels.clone();
            // (Label 0, the change label, is always scanned and not listed.)
            asked.retain(|&m| m != 0);
            asked.sort_unstable();
            asked.dedup();
            if !asked.is_empty() && asked != stored {
                return Err(ClientError::LabelsDiffer { stored, asked });
            }
            let wallet = Wallet::new(keys, network, &stored)?;
            if wallet.scanner().wallet_id() != meta.wallet_id {
                return Err(ClientError::WrongSeed);
            }
            return Ok(Self {
                wallet,
                source,
                store,
                meta,
                config,
            });
        }

        let tip = source.tip()?.height;
        let (birthday, basis) = match config.birthday {
            Some(height) => (height, "given when the wallet was created".to_string()),
            None => (
                tip.max(1),
                format!("the wallet was created when the node's tip was {tip}"),
            ),
        };
        let meta = new_store_meta(&source, wallet.scanner(), birthday, &basis);
        let mut store = SqliteStore::create(&config.store_path)?;
        if let Err(e) = store.initialize(&meta) {
            drop(store);
            let _ = std::fs::remove_file(&config.store_path);
            return Err(e.into());
        }
        Ok(Self {
            wallet,
            source,
            store,
            meta,
            config,
        })
    }

    /// The wallet's network.
    pub fn network(&self) -> Network {
        self.config.network
    }

    /// What the store is bound to: chain, labels, birthday.
    pub fn store_meta(&self) -> &StoreMeta {
        &self.meta
    }

    /// The address to hand out: unlabelled, or with label `m` (the store
    /// must scan for it; label 0 is the change label and is refused).
    pub fn receive_address(&self, label: Option<u32>) -> Result<SilentPaymentAddress, ClientError> {
        if let Some(m) = label.filter(|&m| m != 0) {
            if !self.meta.labels.contains(&m) {
                return Err(ClientError::LabelsDiffer {
                    stored: self
                        .meta
                        .labels
                        .iter()
                        .copied()
                        .filter(|&l| l != 0)
                        .collect(),
                    asked: vec![m],
                });
            }
        }
        Ok(self.wallet.address(label).map_err(WalletError::from)?)
    }

    /// Scans new blocks from the node, follows reorganizations, and brings
    /// the record of pending transactions up to date.
    pub fn sync(&mut self) -> Result<SyncSummary, ClientError> {
        let report = sync(
            &self.source,
            self.wallet.scanner(),
            &mut self.store,
            &SyncOptions::default(),
        )?;
        let settled = self.settle_pending()?;
        Ok(SyncSummary { report, settled })
    }

    fn settle_pending(&self) -> Result<Vec<(PendingTransaction, Settlement)>, ClientError> {
        let path = pending_path(&self.config.store_path);
        let mut pending = PendingSpends::load(&path)?;
        if pending.transactions().is_empty() {
            return Ok(Vec::new());
        }
        let before = pending.clone();
        let checkpoint = self.store.checkpoint()?.map_or(0, |block| block.height);
        let changes = pending.settle(&self.store.spends()?, checkpoint);
        if pending != before {
            pending.save(&path)?;
        }
        Ok(changes)
    }

    /// What the store shows the wallet to hold as of its last scanned block:
    /// outputs detected through full blocks from your node with no recorded
    /// spend and enough confirmations ("unspent"), and — apart — outputs
    /// that are too fresh, or spent by a transaction that is still pending.
    /// Call [`Self::sync`] first; this reads the store only.
    pub fn holdings(&self) -> Result<Holdings, ClientError> {
        let pending = PendingSpends::load(&pending_path(&self.config.store_path))?;
        Ok(holdings(
            &self.store,
            self.config.min_confirmations,
            &pending,
        )?)
    }

    /// Every output detected so far, oldest first.
    pub fn received(&self) -> Result<Vec<DetectedOutput>, ClientError> {
        Ok(self.store.detections()?)
    }

    /// Every spend of a detected output seen in a scanned block.
    pub fn spends(&self) -> Result<Vec<SpentOutput>, ClientError> {
        Ok(self.store.spends()?)
    }

    /// Transactions that were sent and are not settled yet.
    pub fn pending(&self) -> Result<Vec<PendingTransaction>, ClientError> {
        let pending = PendingSpends::load(&pending_path(&self.config.store_path))?;
        Ok(pending.transactions().to_vec())
    }

    /// Forgets a pending transaction that will never confirm, so that its
    /// inputs count as unspent again. Returns whether it was on record.
    /// Only for a transaction you know is gone. If it is still in a mempool,
    /// a later payment built from the same inputs competes with it: one of
    /// the two confirms, and nodes may replace the earlier one with the
    /// later one if that pays more.
    pub fn forget_pending(&mut self, txid: Txid) -> Result<bool, ClientError> {
        let path = pending_path(&self.config.store_path);
        let mut pending = PendingSpends::load(&path)?;
        let found = pending.forget(txid);
        if found {
            pending.save(&path)?;
        }
        Ok(found)
    }

    /// Sets an output aside: it is not spent until released with
    /// [`WalletClient::release`], and is listed under
    /// [`Holdings::held_back`] instead of `unspent`. Returns whether that
    /// changed anything. For an output you do not want to spend together
    /// with your others (spending outputs together shows that they belong
    /// to one wallet).
    ///
    /// # Errors
    ///
    /// [`ClientError::CannotHold`] for an output that is spent, being spent,
    /// only claimed by an index, or not detected at all.
    pub fn hold_back(&mut self, outpoint: OutPoint) -> Result<bool, ClientError> {
        let path = pending_path(&self.config.store_path);
        let mut pending = PendingSpends::load(&path)?;
        if !holdings(&self.store, self.config.min_confirmations, &pending)?.can_hold(outpoint) {
            return Err(ClientError::CannotHold(outpoint));
        }
        let changed = pending.hold_back(outpoint);
        if changed {
            pending.save(&path)?;
        }
        Ok(changed)
    }

    /// Releases an output that was set aside. Returns whether it was.
    pub fn release(&mut self, outpoint: OutPoint) -> Result<bool, ClientError> {
        let path = pending_path(&self.config.store_path);
        let mut pending = PendingSpends::load(&path)?;
        let changed = pending.release(outpoint);
        if changed {
            pending.save(&path)?;
        }
        Ok(changed)
    }

    fn rpc(&self) -> Result<CoreRpc, ClientError> {
        let cookie = self
            .config
            .rpc_cookie_file
            .as_deref()
            .ok_or(ClientError::NoRpc)?;
        let url = self
            .config
            .rpc_url
            .as_deref()
            .unwrap_or(&self.config.core_rest_url);
        let rpc = CoreRpc::new(url, cookie).map_err(ClientError::NotSent)?;
        // The node that is handed the transaction must be on the store's
        // chain.
        rpc.check_chain(self.meta.chain.genesis_hash())
            .map_err(ClientError::NotSent)?;
        Ok(rpc)
    }

    /// Builds and signs a payment of `amount_sats` to `to` — a silent
    /// payment address or an ordinary on-chain address of the wallet's
    /// network — at `fee_rate` satoshi per virtual byte. **Sends nothing.**
    ///
    /// The store is brought to the node's tip first, so that only outputs
    /// the chain shows unspent now are used.
    pub fn prepare_send(
        &mut self,
        to: &str,
        amount_sats: u64,
        fee_rate: u64,
    ) -> Result<PreparedSend, ClientError> {
        let network = self.config.network;
        if let Some(limit_sats) = self.config.amount_limit() {
            if amount_sats > limit_sats {
                return Err(ClientError::AmountAboveLimit {
                    amount_sats,
                    limit_sats,
                    network,
                });
            }
        }
        let destination = Destination::parse(to, network)?;
        let summary = self.sync()?;
        if !summary.report.caught_up {
            return Err(ClientError::NotCaughtUp);
        }
        let held = self.holdings()?;
        let candidates: Vec<DetectedOutput> =
            held.unspent.iter().map(|h| h.output.clone()).collect();
        let built = build_and_sign(
            &self.wallet,
            &self.source,
            &candidates,
            &[Payment {
                to: destination,
                amount_sats,
            }],
            fee_rate,
            &self.config.limits,
        )?;
        Ok(PreparedSend {
            built,
            network,
            wallet_id: self.meta.wallet_id.clone(),
            prepared_at: held.as_of,
        })
    }

    /// Hands a prepared transaction to the node. Call it only after the
    /// user has seen the [`PreparedSend`] and decided to send.
    ///
    /// * `Ok(txid)`: the node took it.
    /// * [`ClientError::NotSent`], [`ClientError::Stale`] and the other
    ///   errors: nothing was sent.
    /// * [`ClientError::OutcomeUnknown`]: not known. The transaction stays
    ///   pending and its inputs are not offered again; see the error.
    pub fn send(&mut self, prepared: PreparedSend) -> Result<Txid, ClientError> {
        // A transaction prepared by a client for another wallet, or for
        // another state of this one (a block came in, something else was
        // sent), is built again, not sent.
        let path = pending_path(&self.config.store_path);
        let stale = || ClientError::Stale {
            prepared_at: prepared.prepared_at.map(|block| block.height),
        };
        let built = &prepared.built;
        let txid = built.txid;
        // Taken: spent by a pending transaction, or held back (possibly
        // after this one was prepared — the hold is the later wish).
        let inputs_taken = |pending: &PendingSpends| {
            pending
                .first_unavailable(built.inputs.iter().map(|i| &i.outpoint))
                .is_some()
        };
        if prepared.network != self.config.network
            || prepared.wallet_id != self.meta.wallet_id
            || self.store.checkpoint()? != prepared.prepared_at
            || inputs_taken(&PendingSpends::load(&path)?)
        {
            return Err(stale());
        }
        let rpc = self.rpc()?;
        let record = PendingTransaction {
            txid,
            inputs: built.inputs.iter().map(|i| i.outpoint).collect(),
            sent_at_height: prepared.prepared_at.map_or(0, |block| block.height),
            confirmed_at: None,
        };
        match rpc.would_accept(&built.transaction) {
            Ok(()) => {}
            // The node has it: it went out before (and was forgotten here,
            // or sent by something else). On record, and said as it is.
            Err(e) if e.means_already_with_node() => {
                let mut pending = PendingSpends::load(&path)?;
                pending.record(record);
                pending.save(&path)?;
                return Err(ClientError::AlreadyWithNode { txid });
            }
            Err(e) => return Err(ClientError::NotSent(e)),
        }

        // On record as pending *before* it leaves: if the answer is lost or
        // this process dies, its inputs must not be offered again. The
        // record is read afresh — the calls above took time, and another
        // program using this store may have sent something meanwhile.
        let mut pending = PendingSpends::load(&path)?;
        if inputs_taken(&pending) {
            return Err(stale());
        }
        pending.record(record);
        pending.save(&path)?;
        match rpc.send(&built.transaction) {
            Ok(_) => Ok(txid),
            // The node said no, or was never asked: take the record back.
            // (If that fails the transaction stays listed as pending, which
            // errs on the side of not spending its inputs again.)
            // Read afresh again, so that a hold or a send recorded meanwhile
            // by another program is not written over. (That narrows the
            // window; the record is not locked, and programs using one store
            // at the same time are not supported: docs/THREAT_MODEL.md.)
            Err(e) if e.means_not_done() => {
                if let Ok(mut pending) = PendingSpends::load(&path) {
                    pending.forget(txid);
                    let _ = pending.save(&path);
                }
                Err(ClientError::NotSent(e))
            }
            Err(cause) => Err(ClientError::OutcomeUnknown { txid, cause }),
        }
    }
}
