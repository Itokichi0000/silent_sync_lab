//! Plain-HTTP access to a service on this machine, shared by the network
//! backends.
//!
//! * The URL must be exactly `http://127.x.y.z:<port>`: an IPv4 loopback
//!   literal with an explicit port. Host names (including `localhost`, whose
//!   resolution depends on the system resolver) and IPv6 forms (which the HTTP
//!   client cannot dial) are refused, so no name lookup ever happens and the
//!   request cannot leave the machine.
//! * Redirects are never followed; only status 200 is accepted.
//! * Response bodies are bounded.
//! * Nothing but the request path leaves the process. The client has no TLS
//!   and no proxy support compiled in.

use super::{SourceError, SourceStats};
use std::{
    io::Read,
    net::Ipv4Addr,
    sync::atomic::{AtomicU64, Ordering},
};

/// Seconds to wait for one response.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
/// Largest response body accepted. A serialized block is at most 4 MB; the
/// JSON forms served by index servers are a small multiple of that.
pub(crate) const MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;
/// Largest status line, and largest header block, accepted.
pub(crate) const MAX_HEADER_BYTES: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct LoopbackHttp {
    /// `http://127.x.y.z:port`, canonical, no trailing slash.
    base: String,
    timeout_secs: u64,
    max_body_bytes: u64,
    requests: AtomicU64,
    bytes: AtomicU64,
}

/// Text taken from outside (a URL, a response body) made safe to print:
/// bounded, single line, no control characters.
pub(crate) fn printable(text: &str) -> String {
    let cleaned: String = text
        .trim()
        .chars()
        .take(200)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    cleaned.trim().to_string()
}

impl LoopbackHttp {
    pub(crate) fn new(url: &str, what: &str) -> Result<Self, SourceError> {
        Ok(Self {
            base: loopback_base_url(url, what)?,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_body_bytes: MAX_BODY_BYTES,
            requests: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_max_body_bytes(mut self, limit: u64) -> Self {
        self.max_body_bytes = limit;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timeout_secs(mut self, seconds: u64) -> Self {
        self.timeout_secs = seconds;
        self
    }

    /// GETs `path` (starting with `/`). Anything but HTTP 200 — including
    /// every redirect — and any body larger than the limit is an error.
    pub(crate) fn get(&self, path: &str) -> Result<Vec<u8>, SourceError> {
        match self.request(path)? {
            (200, body) => Ok(body),
            (status, body) => Err(SourceError::Unavailable(format!(
                "GET {path}: HTTP {status}: {}",
                printable(&String::from_utf8_lossy(&body))
            ))),
        }
    }

    /// Like [`get`](Self::get), but tells "the server answered: not found"
    /// (HTTP 404, `Ok(None)`) apart from every failure to get an answer.
    pub(crate) fn get_if_found(&self, path: &str) -> Result<Option<Vec<u8>>, SourceError> {
        match self.request(path)? {
            (200, body) => Ok(Some(body)),
            (404, _) => Ok(None),
            (status, body) => Err(SourceError::Unavailable(format!(
                "GET {path}: HTTP {status}: {}",
                printable(&String::from_utf8_lossy(&body))
            ))),
        }
    }

    /// One bounded request: status code and body.
    fn request(&self, path: &str) -> Result<(i32, Vec<u8>), SourceError> {
        let unavailable = |e: String| SourceError::Unavailable(format!("GET {path}: {e}"));
        let url = format!("{}{path}", self.base);
        self.requests.fetch_add(1, Ordering::Relaxed);
        let response = minreq::get(&url)
            .with_timeout(self.timeout_secs)
            .with_max_redirects(0)
            .with_max_headers_size(MAX_HEADER_BYTES)
            .with_max_status_line_length(MAX_HEADER_BYTES)
            .send_lazy()
            .map_err(|e| unavailable(e.to_string()))?;
        let status = response.status_code;

        let mut body = Vec::new();
        Read::take(response, self.max_body_bytes + 1)
            .read_to_end(&mut body)
            .map_err(|e| unavailable(e.to_string()))?;
        self.bytes.fetch_add(body.len() as u64, Ordering::Relaxed);
        if body.len() as u64 > self.max_body_bytes {
            return Err(unavailable(format!(
                "response body exceeds {} bytes",
                self.max_body_bytes
            )));
        }
        Ok((status, body))
    }

    pub(crate) fn stats(&self) -> SourceStats {
        SourceStats {
            requests: self.requests.load(Ordering::Relaxed),
            downloaded_bytes: self.bytes.load(Ordering::Relaxed),
        }
    }
}

/// Validates `url` as `http://<IPv4 loopback literal>:<port>[/]` and returns
/// the canonical form without a trailing slash.
pub(crate) fn loopback_base_url(url: &str, what: &str) -> Result<String, SourceError> {
    let refuse = |why: &str| {
        // A URL with user info may carry a password: never echo it.
        let shown = if url.contains('@') {
            "<not shown: contains user info>".to_string()
        } else {
            printable(url)
        };
        SourceError::InvalidData(format!(
            "refusing {what} URL `{shown}`: {why} (expected http://127.0.0.1:<port>)"
        ))
    };
    let rest = url
        .trim()
        .strip_prefix("http://")
        .ok_or_else(|| refuse("only plain http to a local service is supported"))?;
    let authority = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = authority
        .split_once(':')
        .ok_or_else(|| refuse("an explicit port is required"))?;
    let ip: Ipv4Addr = host
        .parse()
        .map_err(|_| refuse("the host must be an IPv4 loopback literal such as 127.0.0.1"))?;
    if !ip.is_loopback() {
        return Err(refuse(
            "host is not loopback; remote services are out of scope",
        ));
    }
    let valid_port = !port.is_empty()
        && port.len() <= 5
        && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u16>().is_ok_and(|p| p != 0);
    if !valid_port {
        return Err(refuse("the port must be a number from 1 to 65535"));
    }
    // Rebuilt from the parsed parts: nothing unparsed reaches the HTTP client.
    Ok(format!("http://{ip}:{port}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_http_to_an_ipv4_loopback_literal_with_port_is_accepted() {
        for (url, base) in [
            ("http://127.0.0.1:18443", "http://127.0.0.1:18443"),
            ("http://127.0.0.1:18443/", "http://127.0.0.1:18443"),
            (" http://127.0.0.9:1 ", "http://127.0.0.9:1"),
            (
                "http://127.255.255.254:65535",
                "http://127.255.255.254:65535",
            ),
        ] {
            assert_eq!(
                loopback_base_url(url, "REST").ok().as_deref(),
                Some(base),
                "{url}"
            );
        }
        for url in [
            // not plain http
            "https://127.0.0.1:18443",
            "HTTP://127.0.0.1:18443",
            "ftp://127.0.0.1:21",
            "127.0.0.1:18443",
            "http://",
            // not loopback
            "http://192.168.1.10:18443",
            "http://0.0.0.0:18443",
            "http://128.0.0.1:18443",
            // names are never resolved
            "http://localhost:18443",
            "http://localhost.:18443",
            "http://example.com:8332",
            "http://127.0.0.1.example.com:18443",
            "http://127.0.0.1.:18443",
            // non-canonical or non-IPv4 forms
            "http://127.1:18443",
            "http://0177.0.0.1:18443",
            "http://2130706433:18443",
            "http://[::1]:18443",
            "http://[127.0.0.1]:18443",
            "http://[127.0.0.1]evil.example:8332",
            "http://[::ffff:127.0.0.1]:18443",
            // port problems
            "http://127.0.0.1",
            "http://127.0.0.1:",
            "http://127.0.0.1:0",
            "http://127.0.0.1:65536",
            "http://127.0.0.1:93646",
            "http://127.0.0.1:-1",
            "http://127.0.0.1:+80",
            "http://127.0.0.1:2810x",
            "http://127.0.0.1:28109%20",
            "http://127.0.0.1:28109\\example.com",
            "http://127.0.0.1:80:80",
            "http://127.0.0.1:１８４４３",
            // anything beyond the authority
            "http://127.0.0.1:18443/rest",
            "http://127.0.0.1:18443/?x=1",
            "http://127.0.0.1:18443#f",
            "http://127.0.0.1:18443 /",
            "http://127.0.0.1:18443\r\nHost: evil",
            "http://user:pass@127.0.0.1:18443",
            "http://127.0.0.1:18443@example.com",
        ] {
            assert!(
                loopback_base_url(url, "REST").is_err(),
                "{url:?} must be refused"
            );
        }
    }

    #[test]
    fn refusal_messages_do_not_echo_credentials_or_control_characters() {
        let message = |url: &str| {
            loopback_base_url(url, "REST")
                .expect_err("refused")
                .to_string()
        };
        let with_password = message("http://alice:hunter2@127.0.0.1:18443");
        assert!(!with_password.contains("hunter2") && !with_password.contains("alice"));
        assert!(with_password.contains("not shown"));
        let injected = message("http://10.0.0.1:1\r\nINJECTED\u{1b}[31m");
        assert!(
            !injected.contains('\r') && !injected.contains('\n') && !injected.contains('\u{1b}')
        );
        let long = message(&format!("http://example.com/{}", "a".repeat(5_000)));
        assert!(long.len() < 500);
    }

    /// One-shot HTTP server answering every request with `body`.
    fn serve_once(body: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        url
    }

    #[test]
    fn response_bodies_are_bounded() {
        let at_limit = LoopbackHttp::new(&serve_once(vec![7u8; 1_000]), "test")
            .expect("url")
            .with_max_body_bytes(1_000);
        assert_eq!(at_limit.get("/x").expect("at the limit").len(), 1_000);
        assert_eq!(at_limit.stats().requests, 1);

        let over = LoopbackHttp::new(&serve_once(vec![7u8; 1_001]), "test")
            .expect("url")
            .with_max_body_bytes(1_000);
        match over.get("/x") {
            Err(SourceError::Unavailable(msg)) => {
                assert!(msg.contains("response body exceeds 1000 bytes"), "{msg}")
            }
            other => panic!("an oversized body must be refused, got {other:?}"),
        }
        // No more than limit + 1 bytes were read.
        assert_eq!(over.stats().downloaded_bytes, 1_001);
    }

    /// A server that accepts the connection and then says nothing must not
    /// hang the client: the request fails once the timeout has passed.
    #[test]
    fn a_silent_server_times_out() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        // Detached: it outlives the assertion below and ends with the process.
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                // Hold the connection open, silently, well past the timeout.
                std::thread::sleep(std::time::Duration::from_secs(4));
                drop(stream);
            }
        });
        let client = LoopbackHttp::new(&url, "test")
            .expect("url")
            .with_timeout_secs(1);
        let started = std::time::Instant::now();
        let result = client.get("/x");
        let waited = started.elapsed();
        assert!(
            matches!(result, Err(SourceError::Unavailable(_))),
            "{result:?}"
        );
        assert!(
            waited < std::time::Duration::from_secs(3),
            "gave up only after {waited:?}"
        );
    }

    /// Serves one raw response (status line, headers and body as given).
    fn serve_raw_once(response: Vec<u8>) -> String {
        use std::io::{BufRead, BufReader, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://{}", listener.local_addr().expect("addr"));
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                let mut line = String::new();
                while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
                    line.clear();
                }
                let _ = stream.write_all(&response);
            }
        });
        url
    }

    /// The body limit alone is not enough: headers and the status line are
    /// read before it and must be bounded too.
    #[test]
    fn response_headers_and_status_line_are_bounded() {
        let get = |response: Vec<u8>| {
            LoopbackHttp::new(&serve_raw_once(response), "test")
                .expect("url")
                .get("/x")
        };
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_vec();
        assert_eq!(get(ok).ok(), Some(b"ok".to_vec()));

        let mut flood = b"HTTP/1.1 200 OK\r\nX-Flood: ".to_vec();
        flood.extend(std::iter::repeat_n(b'a', 4 * MAX_HEADER_BYTES));
        flood.extend_from_slice(b"\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        assert!(
            matches!(get(flood), Err(SourceError::Unavailable(_))),
            "an oversized header must be refused"
        );

        let mut status = b"HTTP/1.1 200 ".to_vec();
        status.extend(std::iter::repeat_n(b'a', 4 * MAX_HEADER_BYTES));
        status.extend_from_slice(b"\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        assert!(
            matches!(get(status), Err(SourceError::Unavailable(_))),
            "an oversized status line must be refused"
        );
    }

    #[test]
    fn printable_strips_control_characters_and_bounds_length() {
        assert_eq!(printable("  ok\tthen\r\nmore\u{7}  "), "ok then  more");
        assert_eq!(printable(&"x".repeat(1_000)).len(), 200);
    }
}
