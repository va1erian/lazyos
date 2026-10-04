//! The fetcher against a local plain-HTTP server.

mod support;

use std::io::Write;
use std::sync::mpsc::Receiver;
use std::thread;
use std::time::{Duration, Instant};

use flate2::write::GzEncoder;
use flate2::Compression;
use lazyweb::fetch::{HttpFetcher, Method, Options, Request};
use support::{respond, Recorder, Server};

const WAIT: Duration = Duration::from_secs(20);

fn fetcher() -> HttpFetcher {
    HttpFetcher::new(Options::default())
}

fn outcome(rx: &Receiver<Result<(), String>>) -> Result<(), String> {
    rx.recv_timeout(WAIT).expect("the fetch reported nothing")
}

#[test]
fn ok_page_is_streamed_with_its_headers() {
    let server = Server::plain(|_, out| {
        respond(
            out,
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8")],
            b"<p>hello</p>",
        )
    });
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().start(Request::get(&server.url("http", "127.0.0.1", "/")), sink);
    assert_eq!(outcome(&rx), Ok(()));
    let record = record.lock().unwrap();
    assert_eq!(record.status, Some(200));
    assert_eq!(record.body, b"<p>hello</p>");
    assert_eq!(
        record.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    assert_eq!(record.header("content-length"), Some("12"));
    let seen = &server.requests.lock().unwrap()[0];
    assert_eq!(seen.header("accept-encoding"), Some("gzip, deflate"));
    assert!(seen.header("user-agent").unwrap().contains("LazyWeb"));
}

#[test]
fn redirects_are_reported_not_followed() {
    let server = Server::plain(|seen, out| match seen.path.as_str() {
        "/old" => respond(out, "302 Found", &[("Location", "/new")], b"moved"),
        _ => respond(out, "200 OK", &[], b"new page"),
    });
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().fetch_here(Request::get(&server.url("http", "127.0.0.1", "/old")), sink);
    assert_eq!(outcome(&rx), Ok(()));
    let record = record.lock().unwrap();
    assert_eq!(record.status, Some(302));
    assert_eq!(record.header("location"), Some("/new"));
    assert_eq!(record.body, b"moved");
    assert_eq!(server.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn gzip_is_decoded_and_its_headers_dropped() {
    let page = b"<html><body>squeezed squeezed squeezed</body></html>".repeat(200);
    let mut gz = GzEncoder::new(Vec::new(), Compression::best());
    gz.write_all(&page).unwrap();
    let packed = gz.finish().unwrap();
    let server = Server::plain(move |_, out| {
        respond(out, "200 OK", &[("Content-Encoding", "gzip")], &packed)
    });
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().fetch_here(Request::get(&server.url("http", "127.0.0.1", "/")), sink);
    assert_eq!(outcome(&rx), Ok(()));
    let record = record.lock().unwrap();
    assert_eq!(record.body, page);
    assert_eq!(record.header("content-encoding"), None);
    assert_eq!(record.header("content-length"), None);
}

#[test]
fn chunked_bodies_are_reassembled() {
    let server = Server::plain(|_, out| {
        let _ = out.write_all(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
        );
        for part in ["<p>one", " two", " three</p>"] {
            let _ = write!(out, "{:x}\r\n{part}\r\n", part.len());
            let _ = out.flush();
            thread::sleep(Duration::from_millis(20));
        }
        let _ = out.write_all(b"0\r\n\r\n");
    });
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().fetch_here(Request::get(&server.url("http", "127.0.0.1", "/")), sink);
    assert_eq!(outcome(&rx), Ok(()));
    let record = record.lock().unwrap();
    assert_eq!(record.body, b"<p>one two three</p>");
    assert_eq!(record.header("transfer-encoding"), None);
}

#[test]
fn an_abort_stops_an_endless_body() {
    let server = Server::plain(|_, out| {
        let _ = out.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n");
        let block = [b'x'; 4096];
        // Endless until the client goes away.
        while out.write_all(&block).and_then(|()| out.flush()).is_ok() {
            thread::sleep(Duration::from_millis(5));
        }
    });
    let (mut sink, rx) = Recorder::new();
    sink.abort_after = Some(10_000);
    let started = Instant::now();
    fetcher().start(Request::get(&server.url("http", "127.0.0.1", "/")), sink);
    assert_eq!(outcome(&rx), Err("aborted".to_string()));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn bodies_over_the_cap_fail() {
    let server = Server::plain(|_, out| respond(out, "200 OK", &[], &[b'y'; 100_000]));
    let fetcher = HttpFetcher::new(Options {
        max_body: 1024 * 1024 / 16, // 64 KiB
        ..Options::default()
    });
    let (sink, rx) = Recorder::new();
    fetcher.fetch_here(Request::get(&server.url("http", "127.0.0.1", "/")), sink);
    let err = outcome(&rx).unwrap_err();
    assert!(err.contains("larger than"), "{err}");
}

#[test]
fn post_sends_body_and_headers_but_not_hop_by_hop_ones() {
    let server = Server::plain(|seen, out| respond(out, "200 OK", &[], &seen.body));
    let request = Request {
        url: server.url("http", "127.0.0.1", "/form"),
        method: Method::Post,
        headers: vec![
            (
                "Content-Type".into(),
                "application/x-www-form-urlencoded".into(),
            ),
            ("Referer".into(), "http://example.com/".into()),
            ("Connection".into(), "upgrade".into()),
            ("User-Agent".into(), "Custom/1.0".into()),
        ],
        body: Some(b"q=lazy+os".to_vec()),
    };
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().fetch_here(request, sink);
    assert_eq!(outcome(&rx), Ok(()));
    assert_eq!(record.lock().unwrap().body, b"q=lazy+os");
    let seen = &server.requests.lock().unwrap()[0];
    assert_eq!(seen.method, "POST");
    assert_eq!(seen.header("referer"), Some("http://example.com/"));
    assert_eq!(seen.header("user-agent"), Some("Custom/1.0"));
    assert_ne!(seen.header("connection"), Some("upgrade"));
}

#[test]
fn head_has_no_body() {
    let server = Server::plain(|_, out| {
        let _ =
            out.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 500\r\nConnection: close\r\n\r\n");
    });
    let request = Request {
        method: Method::Head,
        ..Request::get(&server.url("http", "127.0.0.1", "/"))
    };
    let (sink, rx) = Recorder::new();
    let record = sink.record.clone();
    fetcher().fetch_here(request, sink);
    assert_eq!(outcome(&rx), Ok(()));
    assert!(record.lock().unwrap().body.is_empty());
}

#[test]
fn refused_connections_and_other_schemes_fail() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (sink, rx) = Recorder::new();
    fetcher().fetch_here(Request::get(&format!("http://127.0.0.1:{port}/")), sink);
    assert!(outcome(&rx).is_err());
    let (sink, rx) = Recorder::new();
    fetcher().fetch_here(Request::get("ftp://example.com/"), sink);
    assert!(outcome(&rx).unwrap_err().contains("only http and https"));
}

#[test]
fn many_fetches_run_side_by_side() {
    let server = Server::plain(|seen, out| {
        thread::sleep(Duration::from_millis(50));
        respond(out, "200 OK", &[], seen.path.as_bytes())
    });
    let fetcher = fetcher();
    let waits: Vec<_> = (0..12)
        .map(|i| {
            let (sink, rx) = Recorder::new();
            let record = sink.record.clone();
            fetcher.start(
                Request::get(&server.url("http", "127.0.0.1", &format!("/{i}"))),
                sink,
            );
            (i, rx, record)
        })
        .collect();
    for (i, rx, record) in waits {
        assert_eq!(outcome(&rx), Ok(()));
        assert_eq!(record.lock().unwrap().body, format!("/{i}").as_bytes());
    }
}
