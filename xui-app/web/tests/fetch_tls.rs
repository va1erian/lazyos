//! The fetcher against a local rustls server with a throwaway CA.

mod support;

use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

use lazyweb::fetch::{HttpFetcher, Options, Request, Roots};
use support::{respond, Pki, Recorder, Server};

fn trusting(ca_pem: &str) -> HttpFetcher {
    HttpFetcher::new(Options {
        roots: Roots::Pem(Arc::new(ca_pem.as_bytes().to_vec())),
        ..Options::default()
    })
}

fn outcome(rx: &Receiver<Result<(), String>>) -> Result<(), String> {
    rx.recv_timeout(Duration::from_secs(30))
        .expect("the fetch reported nothing")
}

fn hello_server(pki: &Pki) -> Server {
    Server::tls(pki.server.clone(), |_, out| {
        respond(
            out,
            "200 OK",
            &[("Content-Type", "text/html")],
            b"<p>secure</p>",
        )
    })
}

#[test]
fn https_with_a_trusted_ca_works_and_offers_http11() {
    let pki = Pki::new(&["localhost"]);
    let server = hello_server(&pki);
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    trusting(&pki.ca_pem).fetch_here(Request::get(&server.url("https", "localhost", "/")), sink);
    assert_eq!(outcome(&rx), Ok(()));
    let record = record.lock().unwrap();
    assert_eq!(record.status, Some(200));
    assert_eq!(record.body, b"<p>secure</p>");
    let seen = &server.requests.lock().unwrap()[0];
    assert_eq!(seen.alpn.as_deref(), Some(&b"http/1.1"[..]));
}

#[test]
fn a_certificate_from_an_unknown_ca_is_refused() {
    let pki = Pki::new(&["localhost"]);
    let stranger = Pki::named("Some Other CA", &["localhost"]);
    let server = hello_server(&pki);
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    trusting(&stranger.ca_pem)
        .fetch_here(Request::get(&server.url("https", "localhost", "/")), sink);
    let err = outcome(&rx).unwrap_err();
    assert!(err.contains("not from a trusted authority"), "{err}");
    assert_eq!(record.lock().unwrap().status, None);
    assert!(server.requests.lock().unwrap().is_empty());
}

#[test]
fn a_certificate_for_another_name_is_refused() {
    let pki = Pki::new(&["example.org"]);
    let server = hello_server(&pki);
    let (sink, rx) = Recorder::new();
    trusting(&pki.ca_pem).fetch_here(Request::get(&server.url("https", "localhost", "/")), sink);
    let err = outcome(&rx).unwrap_err();
    assert!(err.contains("another name"), "{err}");
}

#[test]
fn a_missing_bundle_fails_https_with_its_path() {
    let pki = Pki::new(&["localhost"]);
    let server = hello_server(&pki);
    let fetcher = HttpFetcher::new(Options {
        roots: Roots::Bundle("/nonexistent/lazyweb/ca.pem".into()),
        ..Options::default()
    });
    let (sink, rx) = Recorder::new();
    fetcher.fetch_here(Request::get(&server.url("https", "localhost", "/")), sink);
    let err = outcome(&rx).unwrap_err();
    assert!(err.contains("/nonexistent/lazyweb/ca.pem"), "{err}");
}

#[test]
fn https_bodies_stream_until_aborted() {
    let pki = Pki::new(&["localhost"]);
    let server = Server::tls(pki.server.clone(), |_, out| {
        let _ = out.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
        let block = [b'z'; 4096];
        while out.write_all(&block).and_then(|()| out.flush()).is_ok() {
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    let (mut sink, rx) = Recorder::new();
    sink.abort_after = Some(20_000);
    trusting(&pki.ca_pem).start(Request::get(&server.url("https", "localhost", "/")), sink);
    assert_eq!(outcome(&rx), Err("aborted".to_string()));
}
