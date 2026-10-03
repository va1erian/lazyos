//! A tiny HTTP/1.1 test server (plain or TLS) and a recording [`Sink`].

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lazyweb::fetch::Sink;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// What the server read from a client.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    pub method: String,
    pub path: String,
    /// Header names lowercased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The ALPN protocol a TLS client negotiated.
    pub alpn: Option<Vec<u8>>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn Fn(&Seen, &mut dyn Write) + Send + Sync;

/// A server on 127.0.0.1 answering each connection with `handler`.
pub struct Server {
    pub port: u16,
    pub hits: Arc<AtomicUsize>,
    pub requests: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn plain(handler: impl Fn(&Seen, &mut dyn Write) + Send + Sync + 'static) -> Server {
        Server::start(None, Arc::new(handler))
    }

    pub fn tls(
        tls: Arc<ServerConfig>,
        handler: impl Fn(&Seen, &mut dyn Write) + Send + Sync + 'static,
    ) -> Server {
        Server::start(Some(tls), Arc::new(handler))
    }

    fn start(tls: Option<Arc<ServerConfig>>, handler: Arc<Handler>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let (h, r) = (hits.clone(), requests.clone());
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tls, handler, h, r) = (tls.clone(), handler.clone(), h.clone(), r.clone());
                thread::spawn(move || serve(stream, tls, &*handler, &h, &r));
            }
        });
        Server {
            port,
            hits,
            requests,
        }
    }

    pub fn url(&self, scheme: &str, host: &str, path: &str) -> String {
        format!("{scheme}://{host}:{}{path}", self.port)
    }
}

fn serve(
    stream: TcpStream,
    tls: Option<Arc<ServerConfig>>,
    handler: &Handler,
    hits: &AtomicUsize,
    requests: &Mutex<Vec<Seen>>,
) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    match tls {
        None => exchange(stream, None, handler, hits, requests),
        Some(config) => {
            let conn = ServerConnection::new(config).unwrap();
            let mut tls = StreamOwned::new(conn, stream);
            // Finish the handshake first so a refused client shows up as a
            // handshake error here, not as a request.
            while tls.conn.is_handshaking() {
                if tls.conn.complete_io(&mut tls.sock).is_err() {
                    return;
                }
            }
            let alpn = tls.conn.alpn_protocol().map(<[u8]>::to_vec);
            exchange(tls, alpn, handler, hits, requests);
        }
    }
}

fn exchange<S: Read + Write>(
    stream: S,
    alpn: Option<Vec<u8>>,
    handler: &Handler,
    hits: &AtomicUsize,
    requests: &Mutex<Vec<Seen>>,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut words = line.split_whitespace();
    let mut seen = Seen {
        method: words.next().unwrap_or("").to_string(),
        path: words.next().unwrap_or("").to_string(),
        alpn,
        ..Seen::default()
    };
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            seen.headers
                .push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let length: usize = seen
        .header("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    seen.body = vec![0; length];
    let _ = reader.read_exact(&mut seen.body);
    hits.fetch_add(1, Ordering::SeqCst);
    requests.lock().unwrap().push(seen.clone());
    let stream = reader.get_mut();
    handler(&seen, stream);
    let _ = stream.flush();
}

/// A complete response with a `Content-Length` and `Connection: close`.
pub fn respond(out: &mut dyn Write, status: &str, headers: &[(&str, &str)], body: &[u8]) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(body);
}

/// What a fetch reported.
#[derive(Debug, Default)]
pub struct Record {
    pub status: Option<u16>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub chunks: usize,
}

impl Record {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// A [`Sink`] that records everything and reports the outcome on a channel.
#[derive(Clone)]
pub struct Recorder {
    pub record: Arc<Mutex<Record>>,
    pub abort: Arc<AtomicBool>,
    /// Abort once this many body bytes have arrived.
    pub abort_after: Option<usize>,
    done: Sender<Result<(), String>>,
}

impl Recorder {
    pub fn new() -> (Recorder, Receiver<Result<(), String>>) {
        let (done, rx) = mpsc::channel();
        let recorder = Recorder {
            record: Arc::default(),
            abort: Arc::default(),
            abort_after: None,
            done,
        };
        (recorder, rx)
    }
}

impl Sink for Recorder {
    fn status(&self, code: u16) {
        self.record.lock().unwrap().status = Some(code);
    }

    fn header(&self, name: &str, value: &str) {
        let mut record = self.record.lock().unwrap();
        record.headers.push((name.to_string(), value.to_string()));
    }

    fn data(&self, bytes: &[u8]) {
        let mut record = self.record.lock().unwrap();
        record.body.extend_from_slice(bytes);
        record.chunks += 1;
        if self.abort_after.is_some_and(|n| record.body.len() >= n) {
            self.abort.store(true, Ordering::SeqCst);
        }
    }

    fn finish(self) {
        let _ = self.done.send(Ok(()));
    }

    fn fail(self, message: &str) {
        let _ = self.done.send(Err(message.to_string()));
    }

    fn is_aborted(&self) -> bool {
        self.abort.load(Ordering::SeqCst)
    }
}

/// A test PKI: a CA and a leaf for `localhost` and 127.0.0.1 it signed.
pub struct Pki {
    pub ca_pem: String,
    pub server: Arc<ServerConfig>,
}

impl Pki {
    pub fn new(names: &[&str]) -> Pki {
        Pki::named("LazyWeb Test CA", names)
    }

    /// A PKI whose CA is called `ca_name` (two CAs with one name would make
    /// webpki check the leaf against the wrong one: a bad signature, not an
    /// unknown issuer).
    pub fn named(ca_name: &str, names: &[&str]) -> Pki {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, ca_name);
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let names: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        let leaf = CertificateParams::new(names)
            .unwrap()
            .signed_by(&leaf_key, &ca, &ca_key)
            .unwrap();
        let key = PrivateKeyDer::Pkcs8(leaf_key.serialize_der().into());
        let chain: Vec<CertificateDer<'static>> = vec![leaf.der().clone()];
        let mut server = ServerConfig::builder_with_provider(nettls_crypto::provider_arc())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .unwrap();
        server.alpn_protocols = vec![b"http/1.1".to_vec()];
        Pki {
            ca_pem: ca.pem(),
            server: Arc::new(server),
        }
    }
}
