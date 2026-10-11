//! An in-process PDS for the api-thread tests: sign-in, the repo writes and
//! whatever a test adds.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use lazysky_core::proto::StrongRef;
use lazysky_core::{Core, Event, Events, Platform, Request, Roots, Secret, Settings};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct Seen {
    pub path: String,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

type Handler = dyn Fn(&Seen) -> (u16, String) + Send + Sync;

pub struct Server {
    pub port: u16,
    pub seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    pub fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let handler: Arc<Handler> = Arc::new(handler);
        let log = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (handler, log) = (Arc::clone(&handler), Arc::clone(&log));
                thread::spawn(move || {
                    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                    let (mut length, mut content_type) = (0usize, String::new());
                    loop {
                        line.clear();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                            break;
                        }
                        if let Some((n, v)) = line.split_once(':') {
                            match n.trim().to_ascii_lowercase().as_str() {
                                "content-length" => length = v.trim().parse().unwrap_or(0),
                                "content-type" => content_type = v.trim().to_string(),
                                _ => {}
                            }
                        }
                    }
                    let mut body = vec![0; length];
                    let _ = reader.read_exact(&mut body);
                    let seen = Seen {
                        path,
                        content_type,
                        body,
                    };
                    log.lock().unwrap().push(seen.clone());
                    let (status, text) = handler(&seen);
                    let head = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        text.len()
                    );
                    let out = reader.get_mut();
                    let _ = out.write_all(head.as_bytes());
                    let _ = out.write_all(text.as_bytes());
                    let _ = out.flush();
                });
            }
        });
        Server { port, seen }
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    pub fn to(&self, nsid: &str) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|s| s.path.contains(nsid))
            .collect()
    }
}

#[derive(Default)]
pub struct Log(pub Mutex<Vec<String>>);

pub struct Testing(Arc<Log>);

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
    fn open_url(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn log(&self, line: &str) {
        self.0 .0.lock().unwrap().push(line.to_string());
    }
}

pub fn post_view(n: u32, author: &str) -> Value {
    json!({"uri": format!("at://{author}/app.bsky.feed.post/{n}"), "cid": format!("c{n}"),
           "author": {"did": author, "handle": "a.test"}, "record": {"text": format!("post {n}")}})
}

/// A PDS for one test: sign-in, the repo writes, reads. `extra` answers
/// anything else.
pub fn pds(extra: impl Fn(&Seen) -> Option<(u16, String)> + Send + Sync + 'static) -> Server {
    Server::start(move |seen| {
        if seen.path.contains("createSession") {
            return (
                200,
                json!({"accessJwt": "a.b.c", "refreshJwt": "d.e.f",
                       "did": "did:plc:me", "handle": "me.test"})
                .to_string(),
            );
        }
        if let Some(answer) = extra(seen) {
            return answer;
        }
        if seen.path.contains("createRecord") {
            let rec = seen.json();
            let rkey = match rec["collection"].as_str().unwrap_or("") {
                "app.bsky.feed.like" => "3klike",
                "app.bsky.feed.repost" => "3krepost",
                _ => "3kpost",
            };
            return (
                200,
                json!({"uri": format!("at://did:plc:me/{}/{rkey}", rec["collection"].as_str().unwrap()),
                       "cid": "bafynew"})
                .to_string(),
            );
        }
        if seen.path.contains("deleteRecord") {
            return (200, "{}".to_string());
        }
        if seen.path.contains("uploadBlob") {
            return (
                200,
                json!({"blob": {"$type": "blob", "ref": {"$link": "bafyblob"},
                                "mimeType": "image/png", "size": seen.body.len()}})
                .to_string(),
            );
        }
        (
            404,
            r#"{"error":"NotFound","message":"nothing here"}"#.to_string(),
        )
    })
}

pub struct Rig {
    pub core: Core,
    pub events: Events,
    pub log: Arc<Log>,
}

pub fn rig(server: &Server) -> Rig {
    let log: Arc<Log> = Arc::default();
    let settings = Settings {
        service: format!("http://127.0.0.1:{}", server.port),
        ..Settings::default()
    };
    let (core, events) = Core::start(Arc::new(Testing(log.clone())), settings);
    core.send(Request::Login {
        seq: 1,
        handle: "me.test".into(),
        password: Secret::new("pw"),
    });
    assert!(matches!(
        events.wait(Duration::from_secs(20)),
        Some(Event::LoggedIn { .. })
    ));
    Rig { core, events, log }
}

impl Rig {
    pub fn next(&self) -> Event {
        self.events
            .wait(Duration::from_secs(20))
            .expect("an answer")
    }
}

pub fn subject() -> StrongRef {
    StrongRef {
        uri: "at://did:plc:a/app.bsky.feed.post/9".into(),
        cid: "bafypost".into(),
    }
}

pub fn picture(dir: &str, name: &str, w: u32, h: u32) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazysky-{dir}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let img = RgbaImage::from_fn(w, h, |x, y| Rgba([x as u8, y as u8, 99, 255]));
    let mut bytes = Vec::new();
    img.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}
