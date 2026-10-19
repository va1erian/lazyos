//! A scripted TLS server with a throwaway CA, standing in for Fireworks
//! (docs/ai-agent-plan.md §10.1). Each request gets the steps the responder
//! returns for it: bytes sent at once, bytes sent in two parts with a pause
//! (so a client read times out inside a TLS record), or a pause. Nothing here
//! is the real key: tests use `test-key`.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lazyagent_core::{Endpoint, Fireworks, Roots, Secret};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};

pub const KEY: &str = "test-key-0123456789";

pub struct Pki {
    ca_pem: String,
    server: Arc<ServerConfig>,
}

impl Pki {
    pub fn new() -> Pki {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "LazyAgent Test CA");
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = CertificateParams::new(vec!["localhost".to_string()])
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

    pub fn roots(&self) -> Roots {
        Roots::Pem(Arc::new(self.ca_pem.as_bytes().to_vec()))
    }
}

#[derive(Clone)]
pub enum Step {
    Send(Vec<u8>),
    SplitSend(Vec<u8>, Duration),
    Sleep(Duration),
}

/// What the server saw of one request.
#[derive(Clone, Debug, Default)]
pub struct Seen {
    pub request_line: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

pub struct Server {
    pub port: u16,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    pub fn host_port(&self) -> String {
        format!("localhost:{}", self.port)
    }

    pub fn endpoint(&self) -> Endpoint {
        Endpoint::for_testing(&self.host_port())
    }

    pub fn client(&self, pki: &Pki) -> Fireworks {
        Fireworks::new(pki.roots(), Secret::new(KEY), self.endpoint())
    }
}

type Responder = dyn Fn(usize, &Seen) -> Vec<Step> + Send + Sync;

/// Serves every request with `responder(n, request)`, `n` counting from 0.
pub fn serve(
    pki: &Pki,
    responder: impl Fn(usize, &Seen) -> Vec<Step> + Send + Sync + 'static,
) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let responder: Arc<Responder> = Arc::new(responder);
    let counter = Arc::new(AtomicUsize::new(0));
    let (config, log) = (pki.server.clone(), seen.clone());
    thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let (config, log, responder, counter) = (
                config.clone(),
                log.clone(),
                responder.clone(),
                counter.clone(),
            );
            thread::spawn(move || answer(tcp, config, log, responder, counter));
        }
    });
    Server { port, seen }
}

fn answer(
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
    log: Arc<Mutex<Vec<Seen>>>,
    responder: Arc<Responder>,
    counter: Arc<AtomicUsize>,
) {
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut conn = ServerConnection::new(config).unwrap();
    while conn.is_handshaking() {
        if conn.complete_io(&mut tcp).is_err() {
            return;
        }
    }
    let mut seen = Seen::default();
    let mut length = 0usize;
    {
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        let mut reader = BufReader::new(&mut tls);
        let mut first = true;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end().to_string();
            if first {
                seen.request_line = line;
                first = false;
                continue;
            }
            if line.is_empty() {
                break;
            }
            if let Some((n, v)) = line.split_once(':') {
                if n.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
                seen.headers
                    .push((n.trim().to_string(), v.trim().to_string()));
            }
        }
        seen.body = vec![0u8; length];
        reader.read_exact(&mut seen.body).ok();
    }
    let number = counter.fetch_add(1, Ordering::SeqCst);
    let script = responder(number, &seen);
    log.lock().unwrap().push(seen);
    for step in script {
        match step {
            Step::Send(bytes) => {
                conn.writer().write_all(&bytes).unwrap();
                while conn.wants_write() {
                    if conn.write_tls(&mut tcp).is_err() {
                        return;
                    }
                }
                tcp.flush().ok();
            }
            Step::SplitSend(bytes, pause) => {
                conn.writer().write_all(&bytes).unwrap();
                let mut wire = Vec::new();
                while conn.wants_write() {
                    conn.write_tls(&mut wire).unwrap();
                }
                let cut = wire.len() / 2;
                tcp.write_all(&wire[..cut]).ok();
                tcp.flush().ok();
                thread::sleep(pause);
                tcp.write_all(&wire[cut..]).ok();
                tcp.flush().ok();
            }
            Step::Sleep(pause) => thread::sleep(pause),
        }
    }
    // Close politely only after the script, so a short script is a dropped
    // connection from the client's side when it ends mid-stream.
    conn.send_close_notify();
    while conn.wants_write() {
        if conn.write_tls(&mut tcp).is_err() {
            break;
        }
    }
}

/// `data: <json>` as one chunk of a chunked response.
pub fn chunk(text: &str) -> Vec<u8> {
    format!("{:x}\r\n{text}\r\n", text.len()).into_bytes()
}

pub fn sse_head() -> Step {
    Step::Send(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nFireworks-Server-Time-To-First-Token: 0.250\r\n\r\n"
            .to_vec(),
    )
}

/// One SSE event as a chunk.
pub fn event(json: &str) -> Step {
    Step::Send(chunk(&format!("data: {json}\n\n")))
}

pub fn end() -> Step {
    Step::Send(b"0\r\n\r\n".to_vec())
}

pub fn text_delta(text: &str) -> String {
    serde_json::json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":null}]})
        .to_string()
}

pub fn reasoning_delta(text: &str) -> String {
    serde_json::json!({"choices":[{"index":0,"delta":{"reasoning_content":text},"finish_reason":null}]}).to_string()
}

pub fn finish(reason: &str) -> String {
    serde_json::json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]}).to_string()
}

pub fn usage(prompt: u64, completion: u64, reasoning: u64) -> String {
    serde_json::json!({"choices":[],"usage":{"prompt_tokens":prompt,"completion_tokens":completion,
        "completion_tokens_details":{"reasoning_tokens":reasoning}}})
    .to_string()
}

/// A whole successful streamed answer of `text`.
pub fn answer_of(text: &str) -> Vec<Step> {
    vec![
        sse_head(),
        event(&text_delta(text)),
        event(&finish("stop")),
        event(&usage(10, 5, 0)),
        Step::Send(chunk("data: [DONE]\n\n")),
        end(),
    ]
}

/// A complete non-streaming reply with a status, headers and a JSON body.
pub fn status(code: u16, extra_headers: &[(&str, &str)], body: &str) -> Vec<Step> {
    let mut head = format!(
        "HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (n, v) in extra_headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(body);
    vec![Step::Send(head.into_bytes())]
}
