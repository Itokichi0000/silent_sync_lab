//! Handing a transaction to one's own Bitcoin Core node.
//!
//! Core's REST interface only reads; a transaction is sent through JSON-RPC,
//! which needs credentials. They are read from the node's cookie file when
//! the connection is set up, sent to a loopback address only, and never
//! printed, stored or put into an error.

use bdk_sp::bitcoin::{consensus::encode::serialize_hex, BlockHash, Transaction, Txid};
use std::{
    fmt,
    io::{Read as _, Write as _},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::Path,
    str::FromStr,
    time::Duration,
};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

/// Why a call to the node failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RpcError {
    #[error(
        "refusing the RPC URL: expected http://127.0.0.1:<port> (loopback only, and without \
         credentials in the URL — they come from the cookie file)"
    )]
    /// The URL is not a loopback IPv4 address with a port. The URL is not
    /// repeated: it may contain credentials.
    Url,
    #[error("cannot read the cookie file {path}: {reason}")]
    /// The cookie file could not be read or is not a cookie.
    Cookie {
        /// The file.
        path: String,
        /// Why (never the contents).
        reason: String,
    },
    #[error("the node is unavailable: {0}")]
    /// The node could not be reached: no request was sent.
    Unavailable(String),
    #[error("no usable answer from the node: {0}")]
    /// The request was sent (at least in part) and no answer was read: the
    /// connection broke or timed out. **Whether the node acted on the
    /// request is not known.**
    NoAnswer(String),
    #[error("the node at the RPC address is on another chain than the wallet's store")]
    /// The node's genesis block is not the one of the store's chain (for
    /// example a mainnet node on that port).
    WrongChain,
    #[error("the node refused the credentials")]
    /// HTTP 401/403.
    Unauthorized,
    #[error("the node rejected the request: {0}")]
    /// The node answered with an error (for a transaction: why it was not
    /// accepted).
    Rejected(String),
    #[error("unexpected answer from the node: {0}")]
    /// The answer was not what the protocol prescribes. Whether the node
    /// acted on the request is not known.
    Malformed(String),
}

impl RpcError {
    /// Whether this error means the node certainly did **not** act on the
    /// request: it was never sent, or the node said no. For every other
    /// error the outcome is unknown, and a transaction that was being sent
    /// must be treated as possibly sent.
    pub fn means_not_done(&self) -> bool {
        matches!(
            self,
            RpcError::Url
                | RpcError::Cookie { .. }
                | RpcError::Unavailable(_)
                | RpcError::Unauthorized
                | RpcError::Rejected(_)
                | RpcError::WrongChain
        )
    }
}

/// A connection description for the JSON-RPC interface of a local node.
#[derive(Clone)]
pub struct CoreRpc {
    address: SocketAddrV4,
    /// `Basic …` header value. Secret.
    authorization: String,
}

impl fmt::Debug for CoreRpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreRpc")
            .field("address", &self.address)
            .field("authorization", &"<redacted>")
            .finish()
    }
}

/// Standard base64, for the HTTP Basic authorization header.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> shift) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl CoreRpc {
    /// Prepares calls to the node at `url` (`http://127.0.0.1:<port>`), with
    /// the credentials in the node's cookie file.
    pub fn new(url: &str, cookie_file: &Path) -> Result<Self, RpcError> {
        let address = url
            .strip_prefix("http://")
            .map(|rest| rest.trim_end_matches('/'))
            .and_then(|host| SocketAddrV4::from_str(host).ok())
            .filter(|address| address.ip().is_loopback() && *address.ip() != Ipv4Addr::UNSPECIFIED)
            .ok_or(RpcError::Url)?;
        let cookie = |reason: &str| RpcError::Cookie {
            path: cookie_file.display().to_string(),
            reason: reason.to_string(),
        };
        // A cookie is a short line; never read more than that.
        let mut contents = String::new();
        std::fs::File::open(cookie_file)
            .and_then(|file| file.take(1024).read_to_string(&mut contents))
            .map_err(|e| cookie(&e.to_string()))?;
        let contents = contents.trim();
        if !contents.contains(':') || contents.contains(['\n', '\r']) || contents.len() > 512 {
            return Err(cookie("not a `user:password` cookie"));
        }
        Ok(Self {
            address,
            authorization: format!("Basic {}", base64(contents.as_bytes())),
        })
    }

    fn call(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, RpcError> {
        let unavailable = |e: std::io::Error| RpcError::Unavailable(e.to_string());
        // Once bytes have gone out, a failure no longer means "not done".
        let no_answer = |e: std::io::Error| RpcError::NoAnswer(e.to_string());
        let body = serde_json::json!({
            "jsonrpc": "1.0",
            "id": "silent-wallet",
            "method": method,
            "params": params,
        })
        .to_string();
        let mut stream =
            TcpStream::connect_timeout(&self.address.into(), TIMEOUT).map_err(unavailable)?;
        stream
            .set_read_timeout(Some(TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(TIMEOUT)))
            .map_err(unavailable)?;
        let request = format!(
            "POST / HTTP/1.1\r\nHost: {}\r\nAuthorization: {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{}",
            self.address,
            self.authorization,
            body.len(),
            body
        );
        stream.write_all(request.as_bytes()).map_err(no_answer)?;
        let mut response = Vec::new();
        stream
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut response)
            .map_err(no_answer)?;
        if response.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(RpcError::Malformed("the answer is too large".to_string()));
        }
        let text = String::from_utf8_lossy(&response);
        let (head, payload) = text
            .split_once("\r\n\r\n")
            .ok_or_else(|| RpcError::Malformed("no HTTP header".to_string()))?;
        let status = head
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse::<u16>().ok())
            .ok_or_else(|| RpcError::Malformed("no HTTP status".to_string()))?;
        if status == 401 || status == 403 {
            return Err(RpcError::Unauthorized);
        }
        // Core answers errors with status 404/500 and a JSON body.
        let answer: serde_json::Value = serde_json::from_str(payload.trim())
            .map_err(|_| RpcError::Malformed(format!("HTTP status {status} without JSON")))?;
        if !answer["error"].is_null() {
            return Err(RpcError::Rejected(
                answer["error"]["message"]
                    .as_str()
                    .unwrap_or("no message")
                    .to_string(),
            ));
        }
        if status != 200 {
            return Err(RpcError::Malformed(format!("HTTP status {status}")));
        }
        Ok(answer["result"].clone())
    }

    /// Checks that the node is on the chain whose genesis block is
    /// `genesis`. To be called before anything is sent: a node on another
    /// chain (mainnet on that port, say) must not be handed a transaction.
    pub fn check_chain(&self, genesis: BlockHash) -> Result<(), RpcError> {
        let result = self.call("getblockhash", serde_json::json!([0]))?;
        let reported = result
            .as_str()
            .and_then(|text| text.parse::<BlockHash>().ok())
            .ok_or_else(|| RpcError::Malformed("no genesis block hash".to_string()))?;
        if reported == genesis {
            Ok(())
        } else {
            Err(RpcError::WrongChain)
        }
    }

    /// Asks the node whether it would accept the transaction, without
    /// sending it. `Ok(())` if it would.
    pub fn would_accept(&self, transaction: &Transaction) -> Result<(), RpcError> {
        let result = self.call(
            "testmempoolaccept",
            serde_json::json!([[serialize_hex(transaction)]]),
        )?;
        let verdict = &result[0];
        if verdict["allowed"].as_bool() == Some(true) {
            Ok(())
        } else {
            Err(RpcError::Rejected(
                verdict["reject-reason"]
                    .as_str()
                    .unwrap_or("no reason given")
                    .to_string(),
            ))
        }
    }

    /// Hands the transaction to the node for relay. Returns its id as the
    /// node reports it, which must be the transaction's own.
    ///
    /// On an error, ask [`RpcError::means_not_done`]: unless it says so, the
    /// node may have taken the transaction.
    pub fn send(&self, transaction: &Transaction) -> Result<Txid, RpcError> {
        let result = self.call(
            "sendrawtransaction",
            serde_json::json!([serialize_hex(transaction)]),
        )?;
        let reported = result
            .as_str()
            .and_then(|text| text.parse::<Txid>().ok())
            .ok_or_else(|| RpcError::Malformed("no transaction id".to_string()))?;
        if reported != transaction.compute_txid() {
            return Err(RpcError::Malformed(
                "the node reports another transaction id".to_string(),
            ));
        }
        Ok(reported)
    }
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_matches_the_rfc_examples() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("__cookie__:secret/+", "X19jb29raWVfXzpzZWNyZXQvKw=="),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "{plain}");
        }
    }
}
