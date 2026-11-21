//! A minimal stand-in for Bitcoin Core's REST interface, serving the offline
//! fixture chain from memory on a loopback port.
//!
//! It exists so that the Core REST backend can be tested without a node —
//! in particular against a *lying* node: every response can be replaced or
//! removed by the test. It speaks just enough HTTP/1.1 for one GET per
//! connection.

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
        consensus::encode::{deserialize_hex, serialize, VarInt},
        Amount, Block, BlockHash, ScriptBuf, TxOut,
    },
    network::LabNetwork,
    source::fixture::FixtureFile,
};

type Routes = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;

pub struct FakeCoreRest {
    routes: Routes,
    port: u16,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
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

impl FakeCoreRest {
    /// Serves `fixture` the way an honest Bitcoin Core node would.
    pub fn serve(fixture: &FixtureFile) -> Self {
        let network: LabNetwork = fixture.network;
        let mut routes = BTreeMap::new();
        let mut hashes = vec![network.genesis_hash()];
        for fb in &fixture.blocks {
            let block: Block = deserialize_hex(&fb.block_hex).expect("fixture block");
            let hash = block.block_hash();
            let prevouts: Vec<Vec<TxOut>> = fb
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
            routes.insert(format!("/rest/block/{hash}.bin"), serialize(&block));
            routes.insert(
                format!("/rest/spenttxouts/{hash}.bin"),
                encode_spent_outputs(&prevouts),
            );
            routes.insert(
                format!("/rest/headers/{hash}.bin?count=1"),
                serialize(&block.header),
            );
            hashes.push(hash);
        }
        for (height, hash) in hashes.iter().enumerate() {
            routes.insert(
                format!("/rest/blockhashbyheight/{height}.hex"),
                format!("{hash}\n").into_bytes(),
            );
        }
        let tip = hashes.len() - 1;
        routes.insert(
            "/rest/chaininfo.json".to_string(),
            chain_info_json(network.as_str(), tip as u32, hashes[tip]),
        );

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let routes: Routes = Arc::new(Mutex::new(routes));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (routes, stop) = (Arc::clone(&routes), Arc::clone(&stop));
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(stream) = stream {
                        handle(stream, &routes);
                    }
                }
            })
        };
        Self {
            routes,
            port,
            stop,
            thread: Some(thread),
            hashes,
        }
    }

    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Replaces the response for `path`.
    pub fn set(&self, path: &str, body: Vec<u8>) {
        self.routes
            .lock()
            .expect("routes")
            .insert(path.to_string(), body);
    }

    /// Current response for `path`.
    pub fn get(&self, path: &str) -> Vec<u8> {
        self.routes
            .lock()
            .expect("routes")
            .get(path)
            .cloned()
            .expect("route")
    }

    /// Makes `path` answer 404.
    pub fn remove(&self, path: &str) {
        self.routes.lock().expect("routes").remove(path);
    }

    pub fn block_path(&self, height: u32) -> String {
        format!("/rest/block/{}.bin", self.hashes[height as usize])
    }

    pub fn spent_path(&self, height: u32) -> String {
        format!("/rest/spenttxouts/{}.bin", self.hashes[height as usize])
    }

    /// Changes what the node reports as chain name / height / best block.
    pub fn set_chain_info(&self, chain: &str, blocks: u32, best: BlockHash) {
        self.set("/rest/chaininfo.json", chain_info_json(chain, blocks, best));
    }
}

fn chain_info_json(chain: &str, blocks: u32, best: BlockHash) -> Vec<u8> {
    serde_json::json!({
        "chain": chain,
        "blocks": blocks,
        "headers": blocks,
        "bestblockhash": best.to_string(),
        "pruned": false,
        "initialblockdownload": false,
    })
    .to_string()
    .into_bytes()
}

fn handle(mut stream: TcpStream, routes: &Routes) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain headers.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let body = routes.lock().expect("routes").get(path).cloned();
    let (status, body) = match body {
        Some(body) => ("200 OK", body),
        None => ("404 Not Found", b"Not found\r\n".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

impl Drop for FakeCoreRest {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Unblock the accept loop.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
