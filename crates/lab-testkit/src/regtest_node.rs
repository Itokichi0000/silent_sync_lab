//! A throwaway Bitcoin Core node for end-to-end tests.
//!
//! The binary is taken from the `BITCOIND_EXE` environment variable (the
//! convention upstream's test tooling uses); `bitcoin-cli` is expected next to
//! it unless `BITCOIN_CLI_EXE` says otherwise. Nothing is downloaded here: see
//! `scripts/fetch_bitcoind.sh`.
//!
//! The node binds RPC and REST to 127.0.0.1 on a free port, never opens a P2P
//! listener, connects to no peers and keeps its data in a temporary directory
//! that is removed on drop. RPC uses the cookie file inside that directory via
//! `bitcoin-cli`; no credential is ever read or printed by this code.

use std::{
    env,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

/// Which chain the throwaway node runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeChain {
    Regtest,
    /// Mainnet parameters with no network access at all (`-connect=0`,
    /// `-dnsseed=0`, `-listen=0`). Exists only to prove that the lab refuses a
    /// node that reports `main`; it holds nothing but the genesis block.
    OfflineMainnet,
}

pub struct Node {
    child: Option<Child>,
    datadir: PathBuf,
    port: u16,
    cli: PathBuf,
    chain: NodeChain,
}

/// Path of the bitcoind binary, or `None` when `BITCOIND_EXE` is not set.
pub fn bitcoind_exe() -> Option<PathBuf> {
    env::var_os("BITCOIND_EXE").map(PathBuf::from)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free loopback port")
        .port()
}

impl Node {
    /// Starts a node and waits until RPC answers.
    ///
    /// Panics with a clear message when `BITCOIND_EXE` is missing: end-to-end
    /// tests must fail loudly rather than pass without having run.
    pub fn start(chain: NodeChain) -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let bitcoind = bitcoind_exe().expect(
            "BITCOIND_EXE is not set. End-to-end tests need a Bitcoin Core >= 30 binary; \
             see scripts/fetch_bitcoind.sh",
        );
        let cli = env::var_os("BITCOIN_CLI_EXE")
            .map(PathBuf::from)
            .unwrap_or_else(|| bitcoind.with_file_name("bitcoin-cli"));
        assert!(cli.exists(), "bitcoin-cli not found at {}", cli.display());

        // Not guessable in advance, and never an existing directory: the
        // datadir will hold the RPC cookie.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let datadir = env::temp_dir().join(format!(
            "silent-sync-lab-node-{}-{}-{nonce:09}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&datadir).expect("create a fresh datadir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o700))
                .expect("restrict datadir");
        }
        let port = free_port();

        let chain_arg = match chain {
            NodeChain::Regtest => "-chain=regtest",
            NodeChain::OfflineMainnet => "-chain=main",
        };
        // Belt and braces for the mainnet-parameter node: no network activity
        // at all, in addition to having no peers configured.
        let network_arg = match chain {
            NodeChain::Regtest => "-networkactive=1",
            NodeChain::OfflineMainnet => "-networkactive=0",
        };
        let child = Command::new(&bitcoind)
            .arg(chain_arg)
            .arg(network_arg)
            .arg(format!("-datadir={}", datadir.display()))
            .arg(format!("-rpcport={port}"))
            .args([
                "-server=1",
                "-rest=1",
                "-rpcbind=127.0.0.1",
                "-rpcallowip=127.0.0.1",
                "-listen=0",
                "-connect=0",
                "-dnsseed=0",
                "-fixedseeds=0",
                "-natpmp=0",
                "-fallbackfee=0.0001",
                "-printtoconsole=0",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bitcoind");

        let node = Self {
            child: Some(child),
            datadir,
            port,
            cli,
            chain,
        };
        node.wait_until_ready();
        node
    }

    pub fn regtest() -> Self {
        Self::start(NodeChain::Regtest)
    }

    fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            if self.try_cli(&["getblockcount"]).is_ok() {
                return;
            }
            assert!(Instant::now() < deadline, "bitcoind did not become ready");
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Base URL of the REST interface (same port as RPC).
    pub fn rest_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The file holding this node's RPC credentials. Tests hand its path to
    /// the program under test; they never read or print its contents.
    pub fn cookie_file(&self) -> PathBuf {
        match self.chain {
            NodeChain::Regtest => self.datadir.join("regtest").join(".cookie"),
            NodeChain::OfflineMainnet => self.datadir.join(".cookie"),
        }
    }

    fn cli_command(&self) -> Command {
        let mut command = Command::new(&self.cli);
        let chain_arg = match self.chain {
            NodeChain::Regtest => "-chain=regtest",
            NodeChain::OfflineMainnet => "-chain=main",
        };
        command
            .arg(chain_arg)
            .arg(format!("-datadir={}", self.datadir.display()))
            .arg(format!("-rpcport={}", self.port))
            .arg("-rpcconnect=127.0.0.1");
        command
    }

    /// Runs `bitcoin-cli <args>`; `Err` carries stderr.
    pub fn try_cli(&self, args: &[&str]) -> Result<String, String> {
        let output = self
            .cli_command()
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
        }
    }

    /// Runs `bitcoin-cli <args>` and panics on failure.
    pub fn cli(&self, args: &[&str]) -> String {
        self.try_cli(args)
            .unwrap_or_else(|e| panic!("bitcoin-cli {args:?} failed: {e}"))
    }

    pub fn cli_json(&self, args: &[&str]) -> serde_json::Value {
        let text = self.cli(args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("bitcoin-cli {args:?}: {e}: {text}"))
    }

    pub fn block_count(&self) -> u32 {
        self.cli(&["getblockcount"]).parse().expect("block count")
    }

    /// Mines `n` empty-mempool-or-not blocks to `address`; returns their hashes.
    pub fn generate_to(&self, n: u32, address: &str) -> Vec<String> {
        let hashes = self.cli_json(&["generatetoaddress", &n.to_string(), address]);
        hashes
            .as_array()
            .expect("array of hashes")
            .iter()
            .map(|h| h.as_str().expect("hash").to_string())
            .collect()
    }

    /// Mines one block containing exactly `raw_txs` (hex), bypassing mempool
    /// policy. Consensus rules still apply. Returns the block hash.
    pub fn generate_block_with(&self, address: &str, raw_txs: &[String]) -> String {
        let txs = serde_json::to_string(raw_txs).expect("json");
        let result = self.cli_json(&["generateblock", address, &txs]);
        result["hash"].as_str().expect("hash").to_string()
    }

    /// Stops the node and waits for the process to exit.
    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.try_cli(&["stop"]);
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(100))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.datadir);
    }
}
