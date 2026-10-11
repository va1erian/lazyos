//! The api thread end to end against an in-process HTTP server: sign in,
//! a timeline with the bearer token, a token refresh, a rate limit, a bad
//! password, and that no secret reaches an event or a log line.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lazysky_core::{Core, Event, FeedSource, Platform, Request, Roots, Secret, Settings};

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A canned answer.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

fn json(status: u16, body: &str) -> Reply {
    Reply {
        status,
        headers: Vec::new(),
        body: body.to_string(),
    }
}

type Handler = dyn Fn(&Seen, usize) -> Reply + Send + Sync;

/// A plain-HTTP server on 127.0.0.1. `handler` gets each request and its
/// index.
struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(handler: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let log = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, log) = (handler.clone(), log.clone());
                thread::spawn(move || serve(stream, &*handler, &log));
            }
        });
        Server { port, seen }
    }

    fn settings(&self) -> Settings {
        Settings {
            service: format!("http://127.0.0.1:{}", self.port),
            ..Settings::default()
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn serve(stream: std::net::TcpStream, handler: &Handler, log: &Mutex<Vec<Seen>>) {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut words = line.split_whitespace();
    let (method, path) = (
        words.next().unwrap_or("").to_string(),
        words.next().unwrap_or("").to_string(),
    );
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
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    let seen = Seen {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let index = {
        let mut log = log.lock().unwrap();
        log.push(seen.clone());
        log.len() - 1
    };
    let reply = handler(&seen, index);
    let mut head = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (n, v) in &reply.headers {
        head.push_str(&format!("{n}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let out = reader.get_mut();
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(reply.body.as_bytes());
    let _ = out.flush();
}

/// Records what the core logs.
#[derive(Default)]
struct Recording {
    lines: Mutex<Vec<String>>,
}

struct Testing(Arc<Recording>);

impl Platform for Testing {
    fn ca_roots(&self) -> Roots {
        Roots::Pem(Arc::default())
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
    fn log(&self, line: &str) {
        self.0.lines.lock().unwrap().push(line.to_string());
    }
}

const PASSWORD: &str = "abcd-efgh-ijkl-mnop";

fn session_answer(access: &str, refresh: &str, pds: &str) -> String {
    format!(
        r##"{{"accessJwt":"{access}","refreshJwt":"{refresh}","did":"did:plc:me","handle":"me.test",
            "didDoc":{{"service":[{{"id":"#atproto_pds","serviceEndpoint":"{pds}"}}]}}}}"##
    )
}

const TIMELINE: &str = r#"{"cursor":"next","feed":[{"post":{"uri":"at://did:plc:a/app.bsky.feed.post/1",
  "cid":"c","author":{"did":"did:plc:a","handle":"a.test"},"record":{"text":"hello","createdAt":"2026-01-01T00:00:00Z"}}}]}"#;

struct Rig {
    core: Core,
    events: lazysky_core::Events,
    log: Arc<Recording>,
}

fn rig(server: &Server) -> Rig {
    let log: Arc<Recording> = Arc::default();
    let (core, events) = Core::start(Arc::new(Testing(log.clone())), server.settings());
    Rig { core, events, log }
}

impl Rig {
    fn next(&self) -> Event {
        self.events
            .wait(Duration::from_secs(30))
            .expect("the core answered")
    }

    /// The next event that is not a status line.
    fn answer(&self) -> Event {
        loop {
            match self.next() {
                Event::Status { .. } => {}
                other => return other,
            }
        }
    }

    fn login(&self) {
        self.core.send(Request::Login {
            seq: 1,
            handle: "me.test".into(),
            password: Secret::new(PASSWORD),
        });
        assert!(matches!(self.answer(), Event::LoggedIn { .. }));
    }

    fn timeline(&self, seq: u64) {
        self.core.send(Request::Feed {
            seq,
            source: FeedSource::Timeline,
            cursor: None,
        });
    }
}

/// A login service whose answers name `pds` as the account's server, and
/// the PDS itself (`handler`). Returns `(login, pds)`.
fn pair(
    access: &'static str,
    refresh: &'static str,
    handler: impl Fn(&Seen, usize) -> Reply + Send + Sync + 'static,
) -> (Server, Server) {
    let pds = Server::start(handler);
    let url = format!("http://127.0.0.1:{}", pds.port);
    let login = Server::start(move |_, _| json(200, &session_answer(access, refresh, &url)));
    (login, pds)
}

fn paths(server: &Server) -> Vec<String> {
    server
        .requests()
        .iter()
        .map(|s| s.path.split('?').next().unwrap().to_string())
        .collect()
}

#[test]
fn login_then_timeline_with_the_bearer_token_on_the_pds_from_the_did_document() {
    let (login, pds) = pair("access-1", "refresh-1", |_, _| json(200, TIMELINE));
    let rig = rig(&login);
    rig.login();
    rig.timeline(2);
    match rig.answer() {
        Event::Feed {
            seq, page, cursor, ..
        } => {
            assert_eq!(seq, 2);
            assert_eq!(cursor, None);
            assert_eq!(page.items.len(), 1);
            assert_eq!(page.cursor.as_deref(), Some("next"));
        }
        other => panic!("{other:?}"),
    }
    // The login went to the service, the timeline to the PDS.
    let signed_in = login.requests();
    assert_eq!(signed_in.len(), 1);
    assert_eq!(signed_in[0].method, "POST");
    let body: serde_json::Value = serde_json::from_str(&signed_in[0].body).unwrap();
    assert_eq!(body["identifier"], "me.test");
    assert_eq!(body["password"], PASSWORD);
    let timeline = pds.requests();
    assert!(timeline[0]
        .path
        .starts_with("/xrpc/app.bsky.feed.getTimeline?limit=30"));
    assert_eq!(timeline[0].header("authorization"), Some("Bearer access-1"));
    let lines = rig.log.lines.lock().unwrap().clone();
    assert!(
        lines.contains(&"SKY:LOGIN:PASS:me.test".to_string()),
        "{lines:?}"
    );
}

#[test]
fn an_expired_token_is_refreshed_once_and_the_call_retried() {
    let (login, pds) = pair("old-access", "old-refresh", |seen, _| {
        if seen.path.contains("refreshSession") {
            // The refreshed session names no PDS: the current one is kept.
            json(
                200,
                r#"{"accessJwt":"new-access","refreshJwt":"new-refresh","did":"did:plc:me","handle":"me.test"}"#,
            )
        } else if seen.header("authorization") == Some("Bearer new-access") {
            json(200, TIMELINE)
        } else {
            json(
                400,
                r#"{"error":"ExpiredToken","message":"Token has expired"}"#,
            )
        }
    });
    let rig = rig(&login);
    rig.login();
    rig.core.send(Request::Feed {
        seq: 2,
        source: FeedSource::Timeline,
        cursor: Some("abc".into()),
    });
    let answer = rig.answer();
    assert!(matches!(answer, Event::Feed { seq: 2, .. }), "{answer:?}");
    assert_eq!(
        paths(&pds),
        [
            "/xrpc/app.bsky.feed.getTimeline",
            "/xrpc/com.atproto.server.refreshSession",
            "/xrpc/app.bsky.feed.getTimeline",
        ]
    );
    let seen = pds.requests();
    assert_eq!(seen[1].header("authorization"), Some("Bearer old-refresh"));
    assert!(seen[2].path.contains("cursor=abc"));
}

#[test]
fn a_refresh_that_fails_for_the_moment_keeps_the_session() {
    for (status, body) in [(503, "upstream down"), (429, "{}"), (200, "<html>proxy</html>")] {
        let (login, pds) = pair("a", "r", move |seen, _| {
            if seen.path.contains("refreshSession") {
                json(status, body)
            } else {
                json(400, r#"{"error":"ExpiredToken","message":"expired"}"#)
            }
        });
        let rig = rig(&login);
        rig.login();
        rig.timeline(2);
        match rig.answer() {
            Event::Failed { signed_out, .. } => {
                assert!(!signed_out, "a {status} on refresh must not sign the user out")
            }
            other => panic!("{other:?}"),
        }
        // Still signed in: the next call tries again instead of NotSignedIn.
        rig.timeline(3);
        match rig.answer() {
            Event::Failed { signed_out, .. } => assert!(!signed_out),
            other => panic!("{other:?}"),
        }
        assert!(paths(&pds).iter().filter(|p| p.ends_with("refreshSession")).count() >= 2);
    }
}

#[test]
fn a_refresh_the_server_refuses_ends_the_session() {
    let (login, pds) = pair("a", "r", |_, _| {
        json(400, r#"{"error":"ExpiredToken","message":"expired"}"#)
    });
    let rig = rig(&login);
    rig.login();
    rig.timeline(2);
    assert!(matches!(
        rig.answer(),
        Event::Failed {
            seq: 2,
            signed_out: true,
            ..
        }
    ));
    // The session is gone: the next call says so without a request.
    let before = pds.requests().len();
    rig.timeline(3);
    assert!(matches!(
        rig.answer(),
        Event::Failed {
            seq: 3,
            signed_out: true,
            ..
        }
    ));
    assert_eq!(pds.requests().len(), before);
}

#[test]
fn a_bad_password_fails_the_login_without_echoing_it() {
    let server = Server::start(|_, _| {
        json(
            401,
            r#"{"error":"AuthenticationRequired","message":"Invalid identifier or password"}"#,
        )
    });
    let rig = rig(&server);
    let request = Request::Login {
        seq: 1,
        handle: "me.test".into(),
        password: Secret::new(PASSWORD),
    };
    assert!(!format!("{request:?}").contains(PASSWORD));
    rig.core.send(request);
    match rig.answer() {
        Event::LoginFailed { reason, .. } => {
            assert!(
                reason.contains("Invalid identifier or password"),
                "{reason}"
            );
            assert!(!reason.contains(PASSWORD));
        }
        other => panic!("{other:?}"),
    }
    let lines = rig.log.lines.lock().unwrap().join(
        "
",
    );
    assert!(lines.contains("SKY:LOGIN:FAIL:"), "{lines}");
    assert!(!lines.contains(PASSWORD));
}

#[test]
fn a_rate_limit_is_waited_out_with_a_status_line() {
    let (login, _pds) = pair("a", "r", |_, index| {
        if index == 0 {
            Reply {
                status: 429,
                headers: vec![("ratelimit-reset".into(), "0".into())],
                body: r#"{"error":"RateLimitExceeded"}"#.into(),
            }
        } else {
            json(200, TIMELINE)
        }
    });
    let rig = rig(&login);
    rig.login();
    let started = Instant::now();
    rig.timeline(2);
    let mut saw_status = false;
    loop {
        match rig.next() {
            Event::Status { line } => {
                assert!(line.contains("Rate limited"), "{line}");
                saw_status = true;
            }
            Event::Feed { seq: 2, .. } => break,
            other => panic!("{other:?}"),
        }
    }
    assert!(saw_status);
    // A reset in the past waits the one-second minimum.
    assert!(started.elapsed() >= Duration::from_millis(900));
}

#[test]
fn nothing_is_sent_before_signing_in_and_logout_forgets() {
    let server = Server::start(|_, _| json(200, TIMELINE));
    let rig = rig(&server);
    rig.timeline(1);
    assert!(matches!(
        rig.answer(),
        Event::Failed {
            seq: 1,
            signed_out: true,
            ..
        }
    ));
    assert!(server.requests().is_empty());
    rig.core.send(Request::Logout { seq: 2 });
    assert!(matches!(rig.answer(), Event::LoggedOut { seq: 2 }));
}
