//! The core's HTTPS path against a local rustls server with a throwaway CA
//! (modelled on LazyWeb's `tests/support` and `fetch_tls.rs`).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use lazysky_core::{Core, Event, Http, HttpOptions, Platform, Request, Roots, Secret, Settings};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

/// A CA and a leaf for `names` it signed.
struct Pki {
    ca_pem: String,
    server: Arc<ServerConfig>,
}

impl Pki {
    fn named(ca_name: &str, names: &[&str]) -> Pki {
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

    fn roots(&self) -> Roots {
        Roots::Pem(Arc::new(self.ca_pem.as_bytes().to_vec()))
    }
}

/// What the server saw of each request: the request line and the headers.
type Seen = Arc<Mutex<Vec<(String, Vec<(String, String)>)>>>;

/// A TLS server on 127.0.0.1 answering every request with `body`.
fn serve(pki: &Pki, body: &'static [u8]) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let (config, log) = (pki.server.clone(), seen.clone());
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let (config, log) = (config.clone(), log.clone());
            thread::spawn(move || {
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let mut tls = StreamOwned::new(ServerConnection::new(config).unwrap(), stream);
                while tls.conn.is_handshaking() {
                    if tls.conn.complete_io(&mut tls.sock).is_err() {
                        return;
                    }
                }
                let mut reader = BufReader::new(tls);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    return;
                }
                let request_line = line.trim().to_string();
                let mut headers = Vec::new();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                    if let Some((n, v)) = line.split_once(':') {
                        headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
                    }
                }
                let length: usize = headers
                    .iter()
                    .find(|(n, _)| n == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let mut request_body = vec![0; length];
                let _ = reader.read_exact(&mut request_body);
                log.lock().unwrap().push((request_line, headers));
                let out = reader.get_mut();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = out.write_all(head.as_bytes());
                let _ = out.write_all(body);
                let _ = out.flush();
                let _ = reader.read(&mut [0u8; 1]);
            });
        }
    });
    (port, seen)
}

struct Testing(Roots);

impl Platform for Testing {
    fn ca_roots(&self) -> Roots {
        self.0.clone()
    }
    fn app_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn pictures_dir(&self) -> PathBuf {
        std::env::temp_dir()
    }
    fn open_url(&self, _url: &str) -> Result<(), String> {
        Ok(())
    }
    fn log(&self, _line: &str) {}
}

#[test]
fn the_core_signs_in_over_tls_with_a_trusted_ca() {
    let pki = Pki::named("LazySky Test CA", &["localhost"]);
    let answer: &'static [u8] =
        br#"{"accessJwt":"a.b.c","refreshJwt":"d.e.f","did":"did:plc:me","handle":"me.test"}"#;
    let (port, seen) = serve(&pki, answer);
    let settings = Settings {
        service: format!("https://localhost:{port}"),
    };
    let (core, events) = Core::start(Arc::new(Testing(pki.roots())), settings);
    core.send(Request::Login {
        seq: 7,
        handle: "@me.test".into(),
        password: Secret::new("app-pass-word"),
    });
    match events.wait(Duration::from_secs(30)) {
        Some(Event::LoggedIn { seq, account }) => {
            assert_eq!(seq, 7);
            assert_eq!(account.handle, "me.test");
        }
        other => panic!("unexpected: {other:?}"),
    }
    let seen = seen.lock().unwrap();
    assert_eq!(
        seen[0].0,
        "POST /xrpc/com.atproto.server.createSession HTTP/1.1"
    );
    let agent = &seen[0].1.iter().find(|(n, _)| n == "user-agent").unwrap().1;
    assert!(agent.starts_with("LazySky/"), "{agent}");
}

#[test]
fn a_certificate_from_an_unknown_ca_is_refused() {
    let pki = Pki::named("LazySky Test CA", &["localhost"]);
    let stranger = Pki::named("Some Other CA", &["localhost"]);
    let (port, seen) = serve(&pki, b"never read");
    let http = Http::new(stranger.roots(), HttpOptions::default());
    let err = http
        .get(&format!("https://localhost:{port}/"), &[])
        .unwrap_err();
    assert!(err.contains("not from a trusted authority"), "{err}");
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let pki = Pki::named("LazySky Test CA", &["example.org"]);
    let (port, _) = serve(&pki, b"never read");
    let http = Http::new(pki.roots(), HttpOptions::default());
    let err = http
        .get(&format!("https://localhost:{port}/"), &[])
        .unwrap_err();
    assert!(err.contains("another name"), "{err}");
}

#[test]
fn a_body_over_the_cap_is_an_error_not_a_truncation() {
    let pki = Pki::named("LazySky Test CA", &["localhost"]);
    let (port, _) = serve(&pki, &[b'x'; 4096]);
    let options = HttpOptions {
        max_body: 1024,
        ..HttpOptions::default()
    };
    let http = Http::new(pki.roots(), options);
    let err = http
        .get(&format!("https://localhost:{port}/"), &[])
        .unwrap_err();
    assert!(err.contains("longer than 1024"), "{err}");
}

#[test]
fn a_missing_bundle_fails_https_with_its_path() {
    let pki = Pki::named("LazySky Test CA", &["localhost"]);
    let (port, _) = serve(&pki, b"x");
    let http = Http::new(
        Roots::Bundle("/nonexistent/lazysky/ca.pem".into()),
        HttpOptions::default(),
    );
    let err = http
        .get(&format!("https://localhost:{port}/"), &[])
        .unwrap_err();
    assert!(err.contains("/nonexistent/lazysky/ca.pem"), "{err}");
}
