//! A minimal HTTP server that records the headers it receives.
//!
//! It exists to answer one question that nothing else in this repository can:
//! which requests does FFmpeg actually attach a `Cookie` header to? The fake
//! FFmpeg used by `tests/cli.rs` and `tests/native_host.rs` records argv, which
//! proves what we asked FFmpeg to do but not what FFmpeg then puts on the wire.
//! Only a real FFmpeg talking to a real socket answers that, so this server is
//! built to be driven by one (see `tests/cookie_scope.rs`).
//!
//! It is deliberately hand-rolled on `std::net::TcpListener`: the extension
//! ships with no dependencies and the crate keeps its own list short, so a test
//! server is not worth a new one.
//!
//! Two instances give two *distinct hostnames* on the loopback interface —
//! `localhost` and `127.0.0.1`. FFmpeg's cookie matching compares the request's
//! host string, so those two are a different host to it while needing no DNS
//! and no `/etc/hosts` entry.

#![allow(dead_code)]

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

/// One request as the server saw it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// Header names lowercased; values as received.
    pub headers: HashMap<String, String>,
}

impl RecordedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn cookie(&self) -> Option<&str> {
        self.header("cookie")
    }
}

/// What the server answers for a given path.
#[derive(Debug, Clone)]
pub enum Reply {
    /// `200` with this body and content type.
    Body {
        content_type: &'static str,
        body: String,
    },
    /// A body that is not text. Served byte for byte.
    Bytes {
        content_type: &'static str,
        body: Vec<u8>,
    },
    /// `302` to this absolute URL.
    Redirect(String),
    /// An arbitrary status with extra headers, for the answers a CDN gives that
    /// are neither a body nor a redirect — a challenge, most of all.
    Status {
        code: u16,
        reason: &'static str,
        /// Sent verbatim, one per line, as `Name: value`.
        headers: Vec<(&'static str, String)>,
        content_type: &'static str,
        body: String,
    },
}

impl Reply {
    pub fn text(content_type: &'static str, body: impl Into<String>) -> Self {
        Self::Body {
            content_type,
            body: body.into(),
        }
    }

    /// A binary body, for the cases that need FFmpeg to actually decode
    /// something rather than fail on text — real HLS segments, above all.
    pub fn bytes(content_type: &'static str, body: impl Into<Vec<u8>>) -> Self {
        Self::Bytes {
            content_type,
            body: body.into(),
        }
    }
}

/// A loopback HTTP server bound to an ephemeral port.
pub struct HeaderRecorder {
    host: &'static str,
    port: u16,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    received: Arc<Mutex<Vec<RecordedRequest>>>,
    shutdown: Arc<AtomicBool>,
}

impl HeaderRecorder {
    /// Bind to `host` (`"localhost"` or `"127.0.0.1"`) on a free port and serve
    /// until dropped.
    pub fn start(host: &'static str) -> Self {
        Self::try_start_on(host, 0).expect("loopback listener binds on a free port")
    }

    /// Bind to `host` on exactly `port`, or return the reason it could not.
    ///
    /// `port` 0 picks a free one. A fixed port can fail for reasons a test
    /// should report rather than panic on: binding below 1024 needs privileges,
    /// and the port may already be held by something else on the machine.
    pub fn try_start_on(host: &'static str, port: u16) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind((host, port))?;
        let port = listener
            .local_addr()
            .expect("listener has an address")
            .port();
        listener
            .set_nonblocking(true)
            .expect("listener can be polled for shutdown");

        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::new(Mutex::new(HashMap::new()));
        let received: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(AtomicBool::new(false));

        let server = Self {
            host,
            port,
            routes: routes.clone(),
            received: received.clone(),
            shutdown: shutdown.clone(),
        };

        thread::spawn(move || {
            while !shutdown.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let routes = routes.clone();
                        let received = received.clone();
                        thread::spawn(move || serve(stream, &routes, &received));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });

        Ok(server)
    }

    /// Answer `path` (for example `/video.mp4`) with `reply`.
    pub fn route(&self, path: &str, reply: Reply) {
        self.routes
            .lock()
            .expect("routes are not poisoned")
            .insert(path.to_string(), reply);
    }

    /// Absolute URL of `path` on this server, using the hostname it was bound
    /// under so FFmpeg sees the host string we intend.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}:{}{path}", self.host, self.port)
    }

    pub fn host(&self) -> &'static str {
        self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// `host:port`, as it appears in the request's `Host` header.
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// Forget every request recorded so far.
    ///
    /// Lets one server serve several phases of a test. That matters for a
    /// privileged port, which cannot simply be rebound between phases: dropping
    /// the server only signals its thread to stop, so an immediate rebind of the
    /// same port can still fail with `AddrInUse`.
    pub fn clear(&self) {
        self.received
            .lock()
            .expect("received log is not poisoned")
            .clear();
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.received
            .lock()
            .expect("received log is not poisoned")
            .clone()
    }

    /// Requests for exactly `path`.
    pub fn requests_for(&self, path: &str) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|request| request.path == path)
            .collect()
    }
}

impl Drop for HeaderRecorder {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &Mutex<HashMap<String, Reply>>,
    received: &Mutex<Vec<RecordedRequest>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let Some(request) = read_request(&stream) else {
        return;
    };

    let reply = routes
        .lock()
        .expect("routes are not poisoned")
        .get(&request.path)
        .cloned();

    received
        .lock()
        .expect("received log is not poisoned")
        .push(request);

    // Built as bytes because a `Bytes` reply's body is not UTF-8; every other
    // arm renders a string and is appended as its own bytes.
    let mut binary: Option<(String, Vec<u8>)> = None;
    let response = match reply {
        Some(Reply::Bytes { content_type, body }) => {
            binary = Some((
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccept-Ranges: none\r\n\r\n",
                    body.len()
                ),
                body,
            ));
            String::new()
        }
        Some(Reply::Body { content_type, body }) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nAccept-Ranges: none\r\n\r\n{body}",
            body.len()
        ),
        Some(Reply::Redirect(location)) => format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        ),
        Some(Reply::Status {
            code,
            reason,
            headers,
            content_type,
            body,
        }) => {
            let extra: String = headers
                .iter()
                .map(|(name, value)| format!("{name}: {value}\r\n"))
                .collect();
            format!(
                "HTTP/1.1 {code} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                body.len(),
            )
        }
        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    };

    if let Some((headers, body)) = binary {
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(&body);
    } else {
        let _ = stream.write_all(response.as_bytes());
    }
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Write);
}

fn read_request(stream: &TcpStream) -> Option<RecordedRequest> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);

    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let path = target
        .split(['?', '#'])
        .next()
        .unwrap_or(&target)
        .to_string();

    let mut headers = HashMap::new();
    let mut content_length = 0_usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        if name == "content-length" {
            content_length = value.parse().unwrap_or(0);
        }
        headers.insert(name, value);
    }

    // Drain any body so the client is not left writing into a closed socket.
    if content_length > 0 {
        let mut body = vec![0_u8; content_length];
        let _ = reader.read_exact(&mut body);
    }

    Some(RecordedRequest {
        method,
        path,
        headers,
    })
}

/// Minimal `GET` against the fixture server, so its own behaviour can be tested
/// without a real FFmpeg and without adding an HTTP client dependency. Returns
/// the status code and the body. Redirects are *not* followed: the tests that
/// care about redirects want to see each hop.
pub fn get(url: &str, cookie: Option<&str>) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("fixture URLs are http");
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };

    let mut stream = TcpStream::connect(authority).expect("fixture server accepts connections");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout is settable");
    let cookie_line = cookie
        .map(|value| format!("Cookie: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\n{cookie_line}Connection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("request is written");
    stream.flush().expect("request is flushed");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response is readable");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("response carries a status code");
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}
