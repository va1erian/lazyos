//! The streaming path and the allowlist against a local rustls server with a
//! throwaway CA (docs/ai-agent-plan.md §5.2, P0 item 5): reads that time out
//! in the middle of a TLS record lose nothing, Stop is noticed within a poll,
//! and a silent server ends the read after the idle limit.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lazyhttps::{Http, HttpOptions, Policy, Roots, Watch};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};

struct Pki {
    ca_pem: String,
    server: Arc<ServerConfig>,
}

impl Pki {
    fn new(names: &[&str]) -> Pki {
        use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "Stream Test CA");
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

    fn roots(&self) -> Roots {
        Roots::Pem(Arc::new(self.ca_pem.as_bytes().to_vec()))
    }
}

/// One step of a scripted response.
#[derive(Clone)]
enum Step {
    /// Plain bytes, encrypted and sent at once.
    Send(Vec<u8>),
    /// Encrypted, then written to the socket in two parts with a pause
    /// between them, so the client's read times out inside a TLS record.
    SplitSend(Vec<u8>, Duration),
    Sleep(Duration),
}

/// Request lines seen, and the body of the last request.
#[derive(Default)]
struct Seen {
    lines: Vec<String>,
    body: Vec<u8>,
}

const HEAD: &str =
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";

fn chunk(data: &str) -> Vec<u8> {
    format!("{:x}\r\n{data}\r\n", data.len()).into_bytes()
}

fn serve(pki: &Pki, script: Vec<Step>, keep_open: Duration) -> (u16, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let (config, log) = (pki.server.clone(), seen.clone());
    thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let (config, log, script) = (config.clone(), log.clone(), script.clone());
            thread::spawn(move || answer(tcp, config, log, script, keep_open));
        }
    });
    (port, seen)
}

fn answer(
    mut tcp: TcpStream,
    config: Arc<ServerConfig>,
    log: Arc<Mutex<Seen>>,
    script: Vec<Step>,
    keep_open: Duration,
) {
    tcp.set_nodelay(true).ok();
    tcp.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut conn = ServerConnection::new(config).unwrap();
    while conn.is_handshaking() {
        if conn.complete_io(&mut tcp).is_err() {
            return;
        }
    }
    // Read the request head and body through the TLS layer.
    let mut head = Vec::new();
    let mut length = 0usize;
    {
        let mut tls = rustls::Stream::new(&mut conn, &mut tcp);
        let mut reader = BufReader::new(&mut tls);
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                return;
            }
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
            head.push(line);
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).ok();
        let mut seen = log.lock().unwrap();
        seen.lines = head;
        seen.body = body;
    }
    for step in script {
        match step {
            Step::Send(bytes) => {
                conn.writer().write_all(&bytes).unwrap();
                while conn.wants_write() {
                    conn.write_tls(&mut tcp).unwrap();
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
    thread::sleep(keep_open);
}

fn watch(stop: &Arc<AtomicBool>, idle_ms: u64) -> Watch {
    Watch {
        stop: stop.clone(),
        idle: Duration::from_millis(idle_ms),
        poll: Duration::from_millis(50),
        wakes: Arc::default(),
    }
}

fn client(pki: &Pki, port: u16) -> Http {
    let options = HttpOptions {
        policy: Policy::https_hosts(&[&format!("localhost:{port}")]),
        ..HttpOptions::default()
    };
    Http::new(pki.roots(), options)
}

fn read_all(stream: &mut impl Read) -> std::io::Result<String> {
    let mut out = String::new();
    stream.read_to_string(&mut out)?;
    Ok(out)
}

#[test]
fn a_timeout_in_the_middle_of_a_record_loses_nothing() {
    let pki = Pki::new(&["localhost"]);
    let payload = "data: hello\n\n".repeat(200);
    let script = vec![
        Step::Send(HEAD.as_bytes().to_vec()),
        Step::Send(chunk("data: first\n\n")),
        // Several poll periods pass between the halves of one record.
        Step::SplitSend(chunk(&payload), Duration::from_millis(400)),
        Step::SplitSend(chunk("data: [DONE]\n\n"), Duration::from_millis(200)),
        Step::Send(b"0\r\n\r\n".to_vec()),
    ];
    let (port, seen) = serve(&pki, script, Duration::from_millis(100));
    let http = client(&pki, port);
    let stop = Arc::new(AtomicBool::new(false));
    let watching = watch(&stop, 5_000);
    let wakes = watching.wakes.clone();
    let mut response = http
        .post_stream(
            &format!("https://localhost:{port}/v1/chat"),
            &[("content-type", "application/json")],
            b"{\"hi\":1}",
            watching,
        )
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-type"), Some("text/event-stream"));
    let text = read_all(&mut response).unwrap();
    assert_eq!(text, format!("data: first\n\n{payload}data: [DONE]\n\n"));
    // The 400 ms pauses between halves of a record really did time reads out.
    assert!(
        wakes.load(Ordering::Relaxed) >= 6,
        "{} wake-ups",
        wakes.load(Ordering::Relaxed)
    );
    let seen = seen.lock().unwrap();
    assert!(
        seen.lines[0].starts_with("POST /v1/chat HTTP/1.1"),
        "{:?}",
        seen.lines
    );
    assert_eq!(seen.body, b"{\"hi\":1}");
}

#[test]
fn stop_ends_a_stalled_read_within_a_poll() {
    let pki = Pki::new(&["localhost"]);
    let script = vec![
        Step::Send(HEAD.as_bytes().to_vec()),
        Step::Send(chunk("data: one\n\n")),
        Step::Sleep(Duration::from_secs(30)),
    ];
    let (port, _) = serve(&pki, script, Duration::ZERO);
    let http = client(&pki, port);
    let stop = Arc::new(AtomicBool::new(false));
    let mut response = http
        .post_stream(
            &format!("https://localhost:{port}/"),
            &[],
            b"{}",
            watch(&stop, 60_000),
        )
        .unwrap();
    let mut got = [0u8; 11];
    response.read_exact(&mut got).unwrap();
    assert_eq!(&got, b"data: one\n\n");
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let err = response.read(&mut [0u8; 64]).unwrap_err();
    let waited = started.elapsed();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted, "{err}");
    assert!(
        waited < Duration::from_millis(300 + 50 + 250),
        "took {waited:?}"
    );
}

#[test]
fn a_silent_server_is_interrupted_after_the_idle_limit() {
    let pki = Pki::new(&["localhost"]);
    let script = vec![
        Step::Send(HEAD.as_bytes().to_vec()),
        Step::Sleep(Duration::from_secs(30)),
    ];
    let (port, _) = serve(&pki, script, Duration::ZERO);
    let http = client(&pki, port);
    let stop = Arc::new(AtomicBool::new(false));
    let mut response = http
        .post_stream(
            &format!("https://localhost:{port}/"),
            &[],
            b"{}",
            watch(&stop, 600),
        )
        .unwrap();
    let started = Instant::now();
    let err = response.read(&mut [0u8; 64]).unwrap_err();
    let waited = started.elapsed();
    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut, "{err}");
    assert!(waited >= Duration::from_millis(600), "{waited:?}");
    assert!(waited < Duration::from_millis(1_500), "{waited:?}");
}

#[test]
fn a_server_that_never_answers_can_be_stopped_before_the_head() {
    let pki = Pki::new(&["localhost"]);
    let (port, _) = serve(
        &pki,
        vec![Step::Sleep(Duration::from_secs(30))],
        Duration::ZERO,
    );
    let http = client(&pki, port);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let err = http
        .post_stream(
            &format!("https://localhost:{port}/"),
            &[],
            b"{}",
            watch(&stop, 60_000),
        )
        .err()
        .expect("stopped");
    assert!(started.elapsed() < Duration::from_secs(2), "{err}");
}

#[test]
fn the_policy_refuses_other_hosts_schemes_and_user_info() {
    let policy = Policy::https_hosts(&["api.example.test"]);
    assert!(policy.check("https://api.example.test/v1/x").is_ok());
    assert!(policy.check("https://API.example.test:443/v1/x").is_ok());
    for refused in [
        "http://api.example.test/v1/x",
        "https://api.example.test.evil.test/",
        "https://evil.test/",
        "https://api.example.test:8443/",
        "https://api.example.test@evil.test/",
        "https://evil.test@api.example.test/",
        "ftp://api.example.test/",
        "//api.example.test/",
        "not a url",
    ] {
        assert!(policy.check(refused).is_err(), "{refused} was allowed");
    }
    // The default policy keeps LazySky's behaviour.
    assert!(Policy::default().check("http://anything.test/").is_ok());
    assert!(Policy::default().check("ftp://anything.test/").is_err());
}

#[test]
fn a_redirect_is_returned_not_followed_and_other_hosts_are_not_reached() {
    let pki = Pki::new(&["localhost"]);
    let redirect =
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/\r\nContent-Length: 0\r\n\r\n";
    let (port, _) = serve(
        &pki,
        vec![Step::Send(redirect.as_bytes().to_vec())],
        Duration::from_millis(100),
    );
    let http = client(&pki, port);
    let response = http
        .post(&format!("https://localhost:{port}/"), &[], b"{}")
        .unwrap();
    assert_eq!(response.status, 302);
    let refused = http.post("https://127.0.0.1:1/", &[], b"{}").unwrap_err();
    assert!(refused.contains("not on the allowed list"), "{refused}");
    let plain = http.post("http://localhost:1/", &[], b"{}").unwrap_err();
    assert!(plain.contains("only https"), "{plain}");
}
