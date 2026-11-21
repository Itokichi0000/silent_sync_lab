//! A throwaway BlindBit Oracle v2 process for end-to-end tests, indexing a
//! [`Node`](crate::regtest_node::Node).
//!
//! The binary is taken from `BLINDBIT_ORACLE_EXE` (build it with
//! `scripts/build_blindbit_oracle.sh`). It binds HTTP and gRPC to 127.0.0.1 on
//! free ports, reads the node's REST interface, and keeps its database in a
//! temporary directory removed on drop. Regtest only.

use crate::regtest_node::Node;
use std::{
    env,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub struct Oracle {
    child: Option<Child>,
    datadir: PathBuf,
    http_port: u16,
}

pub fn oracle_exe() -> Option<PathBuf> {
    env::var_os("BLINDBIT_ORACLE_EXE").map(PathBuf::from)
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free loopback port")
        .port()
}

impl Oracle {
    /// Indexes `node` from height 1 and serves the result. Panics with a clear
    /// message when `BLINDBIT_ORACLE_EXE` is missing.
    pub fn start(node: &Node) -> Self {
        let exe = oracle_exe().expect(
            "BLINDBIT_ORACLE_EXE is not set. Oracle end-to-end tests need a BlindBit Oracle v2 \
             binary; see scripts/build_blindbit_oracle.sh",
        );
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let datadir = env::temp_dir().join(format!(
            "silent-sync-lab-oracle-{}-{nonce:09}",
            std::process::id()
        ));
        std::fs::create_dir(&datadir).expect("create a fresh oracle datadir");
        let (http_port, grpc_port) = (free_port(), free_port());
        let config = format!(
            "log_level = \"warn\"\n\
             http_host = \"127.0.0.1:{http_port}\"\n\
             grpc_host = \"127.0.0.1:{grpc_port}\"\n\
             chain = \"regtest\"\n\
             core_rest_endpoint = \"{}\"\n\
             sync_start_height = 1\n\
             max_parallel_tweak_computations = 2\n\
             max_parallel_requests = 2\n\
             max_cpu_cores = 2\n\
             tweaks_only = 0\n\
             tweaks_full_basic = 1\n\
             tweaks_full_with_dust_filter = 0\n\
             tweaks_cut_through_with_dust_filter = 0\n",
            node.rest_url()
        );
        std::fs::write(datadir.join("blindbit.toml"), config).expect("write oracle config");

        let command = |subcommand: &str| {
            let mut command = Command::new(&exe);
            command
                .arg(subcommand)
                .arg("--datadir")
                .arg(&datadir)
                // The oracle must reach the node directly, never via a proxy.
                .env("NO_PROXY", "127.0.0.1,localhost")
                .env("no_proxy", "127.0.0.1,localhost")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        };
        let status = command("sync").status().expect("run oracle sync");
        assert!(status.success(), "blindbit-oracle sync failed: {status}");
        let child = command("run").spawn().expect("spawn oracle");

        let oracle = Self {
            child: Some(child),
            datadir,
            http_port,
        };
        oracle.wait_for_height(node.block_count());
        oracle
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.http_port)
    }

    fn info_height(&self) -> Option<u32> {
        // A tiny HTTP GET without pulling an HTTP client into the testkit API.
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", self.http_port)).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        stream
            .write_all(b"GET /info HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .ok()?;
        let mut response = String::new();
        stream.read_to_string(&mut response).ok()?;
        let body = &response[response.find('{')?..=response.rfind('}')?];
        let info: serde_json::Value = serde_json::from_str(body).ok()?;
        u32::try_from(info["height"].as_u64()?).ok()
    }

    /// Waits until the oracle reports having indexed up to `height`.
    pub fn wait_for_height(&self, height: u32) {
        self.wait_for_height_within(height, Duration::from_secs(120));
    }

    /// Like [`wait_for_height`](Self::wait_for_height) with a chosen limit
    /// (indexing a long chain takes a while).
    pub fn wait_for_height_within(&self, height: u32, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            if self.info_height().is_some_and(|h| h >= height) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the oracle did not reach height {height} (at {:?})",
                self.info_height()
            );
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Oracle {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.datadir);
    }
}
