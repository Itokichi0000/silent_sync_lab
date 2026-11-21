//! In-process stand-ins for the HTTP services the lab talks to, serving data
//! from memory on a loopback port:
//!
//! * [`FakeCoreRest`] — Bitcoin Core's REST interface;
//! * [`FakeOracle`] — BlindBit Oracle v2's HTTP JSON interface.
//!
//! They exist so that the network backends can be tested without the real
//! programs — in particular against services that *lie*: every response can be
//! replaced, removed or given another status by the test. The server speaks
//! just enough HTTP/1.1 for one GET per connection and records every request.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    net::{TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
};
use sync_core::{
    bitcoin::{
        blockdata::constants::genesis_block,
        consensus::encode::{deserialize_hex, serialize, VarInt},
        Amount, Block, BlockHash, ScriptBuf, TxOut,
    },
    network::LabNetwork,
    source::{fixture::FixtureFile, Evidence, IndexedTx},
};

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

#[derive(Default)]
struct State {
    routes: BTreeMap<String, Reply>,
    requests: Vec<String>,
}

/// A tiny HTTP server with a mutable route table.
pub struct FakeHttp {
    state: Arc<Mutex<State>>,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Default for FakeHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeHttp {
    pub fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let state = Arc::new(Mutex::new(State::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (state, stop) = (Arc::clone(&state), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        handle(stream, &state);
                    }
                }
            })
        };
        Self {
            state,
            port,
            stop,
            thread: Some(thread),
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// Answers `path` (including any query string) with 200 and `body`.
    pub fn set(&self, path: &str, body: Vec<u8>) {
        self.set_reply(path, 200, &[], body);
    }

    /// Answers `path` with an arbitrary status and extra headers.
    pub fn set_reply(&self, path: &str, status: u16, headers: &[(&str, &str)], body: Vec<u8>) {
        let reply = Reply {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body,
        };
        self.state
            .lock()
            .expect("state")
            .routes
            .insert(path.to_string(), reply);
    }

    /// Current body for `path`.
    pub fn get(&self, path: &str) -> Vec<u8> {
        self.state
            .lock()
            .expect("state")
            .routes
            .get(path)
            .expect("route")
            .body
            .clone()
    }

    /// Makes `path` answer 404.
    pub fn remove(&self, path: &str) {
        self.state.lock().expect("state").routes.remove(path);
    }

    /// Forgets every route (all paths answer 404 until set again).
    pub fn clear(&self) {
        self.state.lock().expect("state").routes.clear();
    }

    /// Every request path received so far, in order.
    pub fn requests(&self) -> Vec<String> {
        self.state.lock().expect("state").requests.clone()
    }
}

fn handle(mut stream: TcpStream, state: &Arc<Mutex<State>>) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
        line.clear();
    }
    let Some(path) = request_line.split_whitespace().nth(1) else {
        return; // the wake-up connection made on drop
    };
    let reply = {
        let mut state = state.lock().expect("state");
        state.requests.push(path.to_string());
        state.routes.get(path).cloned()
    }
    .unwrap_or(Reply {
        status: 404,
        headers: Vec::new(),
        body: b"Not found\r\n".to_vec(),
    });
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&reply.body);
    let _ = stream.flush();
}

impl Drop for FakeHttp {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Bitcoin Core REST
// ---------------------------------------------------------------------------

/// Headers per `/rest/headers` response, as the Core backend requests them.
const HEADERS_PER_REQUEST: usize = 2000;

pub struct FakeCoreRest {
    pub http: FakeHttp,
    /// Block hashes by height (index 0 is the genesis block).
    pub hashes: Vec<BlockHash>,
}

/// Body of `/rest/spenttxouts/<hash>.bin` for the given per-transaction prevouts.
pub fn encode_spent_outputs(prevouts: &[Vec<TxOut>]) -> Vec<u8> {
    let mut bytes = serialize(&VarInt(prevouts.len() as u64));
    for tx_prevouts in prevouts {
        bytes.extend(serialize(tx_prevouts));
    }
    bytes
}

/// Decodes a fixture file into blocks and their prevouts.
pub fn fixture_blocks(fixture: &FixtureFile) -> Vec<(Block, Vec<Vec<TxOut>>)> {
    fixture
        .blocks
        .iter()
        .map(|fb| {
            let block: Block = deserialize_hex(&fb.block_hex).expect("fixture block");
            let prevouts = fb
                .prevouts
                .iter()
                .map(|tx| {
                    tx.iter()
                        .map(|p| TxOut {
                            value: Amount::from_sat(p.value_sats),
                            script_pubkey: ScriptBuf::from_hex(&p.script_pubkey_hex)
                                .expect("script"),
                        })
                        .collect()
                })
                .collect();
            (block, prevouts)
        })
        .collect()
}

impl FakeCoreRest {
    /// Serves `fixture` the way an honest Bitcoin Core node would.
    pub fn serve(fixture: &FixtureFile) -> Self {
        Self::serve_blocks(fixture.network, &fixture_blocks(fixture))
    }

    /// Serves arbitrary blocks as heights 1.. on top of `network`'s genesis
    /// block — whether or not they actually build on it.
    pub fn serve_blocks(network: LabNetwork, blocks: &[(Block, Vec<Vec<TxOut>>)]) -> Self {
        let mut node = Self {
            http: FakeHttp::new(),
            hashes: Vec::new(),
        };
        node.replace_chain(network, blocks);
        node
    }

    /// Makes the same server serve a different chain from now on, as a node
    /// does after a reorganization.
    pub fn replace_chain(&mut self, network: LabNetwork, blocks: &[(Block, Vec<Vec<TxOut>>)]) {
        self.http.clear();
        let genesis = genesis_block(network.to_bitcoin());
        let mut headers = vec![genesis.header];
        let mut hashes = vec![genesis.block_hash()];
        for (block, prevouts) in blocks {
            let hash = block.block_hash();
            self.http
                .set(&format!("/rest/block/{hash}.bin"), serialize(block));
            self.http.set(
                &format!("/rest/spenttxouts/{hash}.bin"),
                encode_spent_outputs(prevouts),
            );
            headers.push(block.header);
            hashes.push(hash);
        }
        for (height, hash) in hashes.iter().enumerate() {
            self.http.set(
                &format!("/rest/blockhashbyheight/{height}.hex"),
                format!("{hash}\n").into_bytes(),
            );
            let upto = (height + HEADERS_PER_REQUEST).min(headers.len());
            let body: Vec<u8> = headers[height..upto].iter().flat_map(serialize).collect();
            self.http.set(&Self::headers_path_for(*hash), body);
        }
        let tip = hashes.len() - 1;
        self.hashes = hashes;
        self.set_chain_info(network.as_str(), tip as u32, self.hashes[tip]);
    }

    pub fn url(&self) -> String {
        self.http.url()
    }

    pub fn set(&self, path: &str, body: Vec<u8>) {
        self.http.set(path, body);
    }

    pub fn get(&self, path: &str) -> Vec<u8> {
        self.http.get(path)
    }

    pub fn remove(&self, path: &str) {
        self.http.remove(path);
    }

    pub fn block_path(&self, height: u32) -> String {
        format!("/rest/block/{}.bin", self.hashes[height as usize])
    }

    pub fn spent_path(&self, height: u32) -> String {
        format!("/rest/spenttxouts/{}.bin", self.hashes[height as usize])
    }

    fn headers_path_for(hash: BlockHash) -> String {
        format!("/rest/headers/{hash}.bin?count={HEADERS_PER_REQUEST}")
    }

    /// Path of the "headers from this height upward" response.
    pub fn headers_path(&self, height: u32) -> String {
        Self::headers_path_for(self.hashes[height as usize])
    }

    /// Changes what the node reports as chain name / height / best block.
    pub fn set_chain_info(&self, chain: &str, blocks: u32, best: BlockHash) {
        let body = serde_json::json!({
            "chain": chain,
            "blocks": blocks,
            "headers": blocks,
            "bestblockhash": best.to_string(),
            "pruned": false,
            "initialblockdownload": false,
        });
        self.http
            .set("/rest/chaininfo.json", body.to_string().into_bytes());
    }
}

// ---------------------------------------------------------------------------
// BlindBit Oracle v2 (HTTP JSON)
// ---------------------------------------------------------------------------

pub struct FakeOracle {
    pub http: FakeHttp,
}

/// JSON body of `/full-block/<height>` in the shape observed from BlindBit
/// Oracle v2 (commit f9ab7fd).
pub fn oracle_full_block_json(height: u32, hash: Option<BlockHash>, txs: &[IndexedTx]) -> Vec<u8> {
    let index: Vec<serde_json::Value> = txs
        .iter()
        .map(|tx| {
            serde_json::json!({
                "txid": tx.txid.to_string(),
                "tweak": tx.tweak.to_string(),
                "inputs": [],
                "utxos": tx.outputs.iter().map(|o| serde_json::json!({
                    "vout": o.vout,
                    "amount": o.value_sats,
                    "pubkey": o.output_key.iter().map(|b| format!("{b:02x}")).collect::<String>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({
        "block_identifier": {
            "block_hash": hash.map(|h| h.to_string()).unwrap_or_default(),
            "block_height": height,
        },
        "index": if hash.is_some() { serde_json::Value::Array(index) } else { serde_json::Value::Null },
    })
    .to_string()
    .into_bytes()
}

impl FakeOracle {
    /// Serves a tweak index for the given blocks (heights must start at 1 and
    /// be contiguous), the way the real oracle was observed to: heights it
    /// does not have are answered with HTTP 200, an empty hash and `null`.
    pub fn serve(network: &str, blocks: &[sync_core::source::BlockEvidence]) -> Self {
        let http = FakeHttp::new();
        let tip = blocks.last().map_or(0, |b| b.block.height);
        for block in blocks {
            let Evidence::TweakIndex(txs) = &block.evidence else {
                panic!("FakeOracle serves tweak-index evidence");
            };
            http.set(
                &format!("/full-block/{}", block.block.height),
                oracle_full_block_json(block.block.height, Some(block.block.hash), txs),
            );
        }
        for missing in std::iter::once(0).chain(tip + 1..tip + 20) {
            http.set(
                &format!("/full-block/{missing}"),
                oracle_full_block_json(missing, None, &[]),
            );
        }
        let oracle = Self { http };
        oracle.set_info(network, tip, false);
        oracle
    }

    pub fn url(&self) -> String {
        self.http.url()
    }

    pub fn set_info(&self, network: &str, height: u32, tweaks_only: bool) {
        let body = serde_json::json!({
            "network": network,
            "height": height,
            "tweaks_only": tweaks_only,
            "tweaks_full_basic": true,
            "tweaks_full_with_dust_filter": false,
            "tweaks_cut_through_with_dust_filter": false,
        });
        self.http.set("/info", body.to_string().into_bytes());
    }

    pub fn block_path(height: u32) -> String {
        format!("/full-block/{height}")
    }
}
