//! Net Tools' web client and server, on worker threads over plain `std::net`
//! (the Linux ABI's `AF_INET` shim, docs/architecture/networking.md, N5).
//!
//! The UI thread never blocks on a socket: a fetch runs on its own thread and
//! leaves its outcome in a slot the UI polls on its timer; the server thread
//! accepts with a non-blocking listener so it can notice a stop request, and
//! records what it served in shared state. Threads touch only sockets and
//! these shared values, never the UI's Messenger endpoints (on LazyOS a thread
//! has its own descriptor table). Every read is bounded in size and time.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::http::{self, PageInfo, Summary, Url};
use crate::sys;

/// How long a fetch may take to connect, and to wait for each read.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);
/// The most of a response a fetch keeps.
const MAX_RESPONSE: usize = 512 * 1024;
/// The most of a request head the server reads.
const MAX_REQUEST: usize = 8 * 1024;
/// How long the server waits for a visitor's request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the idle server looks for a connection or a stop request.
const ACCEPT_POLL_MILLIS: u64 = 50;
/// Log lines the server keeps.
const LOG_LINES: usize = 8;

/// The outcome of one fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    pub summary: Summary,
    /// Bytes received, headers included.
    pub bytes: usize,
    /// Wall time in milliseconds (10 ms resolution).
    pub millis: u64,
}

/// A fetch in flight.
pub struct Fetch {
    slot: Arc<Mutex<Option<Result<Fetched, String>>>>,
}

impl Fetch {
    /// Fetch `url` from `addr` (already resolved) on a new thread.
    pub fn start(addr: Ipv4Addr, url: Url) -> Result<Fetch, String> {
        let slot = Arc::new(Mutex::new(None));
        let out = Arc::clone(&slot);
        std::thread::Builder::new()
            .name("fetch".into())
            .spawn(move || {
                let outcome = fetch(SocketAddrV4::new(addr, url.port), &url);
                *out.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
            })
            .map_err(|e| format!("could not start a thread: {e}"))?;
        Ok(Fetch { slot })
    }

    /// The outcome, once (`None` while it runs).
    pub fn take(&self) -> Option<Result<Fetched, String>> {
        self.slot.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

fn fetch(addr: SocketAddrV4, url: &Url) -> Result<Fetched, String> {
    let started = sys::clock_ticks();
    let mut stream = TcpStream::connect_timeout(&SocketAddr::V4(addr), FETCH_TIMEOUT)
        .map_err(|e| format!("connect to {addr}: {e}"))?;
    stream
        .set_read_timeout(Some(FETCH_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(http::request(url).as_bytes())
        .map_err(|e| format!("send: {e}"))?;
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    while response.len() < MAX_RESPONSE {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => response.extend_from_slice(&buf[..n.min(MAX_RESPONSE - response.len())]),
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            // A server that stops sending without closing still gave an answer.
            Err(e) if !response.is_empty() && is_timeout(&e) => break,
            Err(e) => return Err(format!("receive: {e}")),
        }
    }
    Ok(Fetched {
        summary: http::summarize(&response),
        bytes: response.len(),
        millis: sys::clock_ticks().saturating_sub(started) * 10,
    })
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// What the server has done, shared with the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerState {
    /// `Some(Ok(port))` once listening, `Some(Err(..))` if it could not.
    pub listening: Option<Result<u16, String>>,
    /// Requests answered.
    pub hits: u64,
    /// Recent requests, newest last: `10.0.2.2:51234  GET / HTTP/1.1`.
    pub log: Vec<String>,
    /// Set when the thread has ended.
    pub stopped: bool,
}

/// A running web server.
pub struct Server {
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<ServerState>>,
    page: Arc<Mutex<PageInfo>>,
}

impl Server {
    /// Listen on every address, port `port`, on a new thread.
    pub fn start(port: u16) -> Result<Server, String> {
        let server = Server {
            stop: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(ServerState::default())),
            page: Arc::new(Mutex::new(PageInfo::default())),
        };
        let (stop, state, page) = (
            Arc::clone(&server.stop),
            Arc::clone(&server.state),
            Arc::clone(&server.page),
        );
        std::thread::Builder::new()
            .name("httpd".into())
            .spawn(move || {
                serve(port, &stop, &state, &page);
                lock(&state).stopped = true;
            })
            .map_err(|e| format!("could not start a thread: {e}"))?;
        Ok(server)
    }

    /// A copy of what the server has done so far.
    pub fn state(&self) -> ServerState {
        lock(&self.state).clone()
    }

    /// What the next pages show.
    pub fn set_page(&self, info: PageInfo) {
        *lock(&self.page) = info;
    }

    /// Ask the thread to stop (it notices within one accept poll).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn serve(port: u16, stop: &AtomicBool, state: &Mutex<ServerState>, page: &Mutex<PageInfo>) {
    let listener = match TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port)) {
        Ok(listener) => listener,
        Err(e) => {
            lock(state).listening = Some(Err(format!("listen on port {port}: {e}")));
            return;
        }
    };
    // Without a non-blocking accept the thread still serves; it only notices
    // a stop request at the next visitor.
    let polling = listener.set_nonblocking(true).is_ok();
    lock(state).listening = Some(Ok(port));
    println!("NETTOOLS:SERVER:LISTENING port={port} polling={polling}");
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => answer(stream, peer, state, page),
            Err(e) if e.kind() == ErrorKind::WouldBlock => sys::sleep_millis(ACCEPT_POLL_MILLIS),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => {
                push_log(state, format!("accept failed: {e}"));
                sys::sleep_millis(ACCEPT_POLL_MILLIS * 4);
            }
        }
    }
}

/// Read one request head and answer it with the page.
fn answer(
    mut stream: TcpStream,
    peer: SocketAddr,
    state: &Mutex<ServerState>,
    page: &Mutex<PageInfo>,
) {
    // An accepted socket may inherit the listener's non-blocking mode.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let mut request = Vec::new();
    let mut buf = [0u8; 1024];
    while request.len() < MAX_REQUEST && !http::head_complete(&request) {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => request.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let line = http::request_line(&request);
    let hits = lock(state).hits + 1;
    let info = lock(page).clone();
    let sent = stream.write_all(&http::page(&info, &peer.to_string(), hits));
    let _ = stream.shutdown(std::net::Shutdown::Both);
    match sent {
        Ok(()) => {
            lock(state).hits = hits;
            println!("NETTOOLS:SERVER:HIT n={hits} from={peer}");
            push_log(state, format!("{peer}  {line}"));
        }
        Err(e) => push_log(state, format!("{peer}  {line}  (send failed: {e})")),
    }
}

fn push_log(state: &Mutex<ServerState>, line: String) {
    let mut state = lock(state);
    state.log.push(line);
    let excess = state.log.len().saturating_sub(LOG_LINES);
    state.log.drain(..excess);
}
