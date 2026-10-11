//! Reads and writes through the api thread against an in-process server:
//! like and unlike, delete (own records only), a reply with a picture, the
//! picture limits, a thread, the people lists and the Raw view.

use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use image::{ImageFormat, Rgba, RgbaImage};
use lazysky_core::proto::{StrongRef, Thread};
use lazysky_core::{ActorList, Core, Event, Events, Platform, Request, Roots, Secret, Settings};
use serde_json::{json, Value};

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    content_type: String,
    body: Vec<u8>,
}

impl Seen {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

type Handler = dyn Fn(&Seen) -> (u16, String) + Send + Sync;

struct Server {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    fn start(handler: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Server {
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

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn to(&self, nsid: &str) -> Vec<Seen> {
        self.requests()
            .into_iter()
            .filter(|s| s.path.contains(nsid))
            .collect()
    }
}

#[derive(Default)]
struct Log(Mutex<Vec<String>>);

struct Testing(Arc<Log>);

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

fn post_view(n: u32, author: &str) -> Value {
    json!({"uri": format!("at://{author}/app.bsky.feed.post/{n}"), "cid": format!("c{n}"),
           "author": {"did": author, "handle": "a.test"}, "record": {"text": format!("post {n}")}})
}

/// A PDS for one test: sign-in, the repo writes, reads. `extra` answers
/// anything else.
fn pds(extra: impl Fn(&Seen) -> Option<(u16, String)> + Send + Sync + 'static) -> Server {
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

struct Rig {
    core: Core,
    events: Events,
    log: Arc<Log>,
}

fn rig(server: &Server) -> Rig {
    let log: Arc<Log> = Arc::default();
    let settings = Settings {
        service: format!("http://127.0.0.1:{}", server.port),
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
    fn next(&self) -> Event {
        self.events
            .wait(Duration::from_secs(20))
            .expect("an answer")
    }
}

fn subject() -> StrongRef {
    StrongRef {
        uri: "at://did:plc:a/app.bsky.feed.post/9".into(),
        cid: "bafypost".into(),
    }
}

fn picture(dir: &str, name: &str, w: u32, h: u32) -> PathBuf {
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

#[test]
fn a_like_is_written_as_a_record_and_can_be_undone() {
    let server = pds(|_| None);
    let rig = rig(&server);
    rig.core.send(Request::Like {
        seq: 2,
        subject: subject(),
    });
    let Event::Created { seq, uri, cid } = rig.next() else {
        panic!("not created")
    };
    assert_eq!((seq, cid.as_str()), (2, "bafynew"));
    assert_eq!(uri, "at://did:plc:me/app.bsky.feed.like/3klike");
    let sent = server.to("createRecord")[0].json();
    assert_eq!(sent["repo"], "did:plc:me");
    assert_eq!(sent["collection"], "app.bsky.feed.like");
    assert_eq!(sent["record"]["subject"]["uri"], subject().uri);
    assert_eq!(sent["record"]["subject"]["cid"], "bafypost");
    assert!(sent["record"]["createdAt"].as_str().unwrap().ends_with('Z'));

    rig.core.send(Request::DeleteRecord { seq: 3, uri });
    assert!(matches!(rig.next(), Event::Deleted { seq: 3, .. }));
    let del = server.to("deleteRecord")[0].json();
    assert_eq!(del["collection"], "app.bsky.feed.like");
    assert_eq!(del["rkey"], "3klike");
    let lines = rig.log.0.lock().unwrap().join("\n");
    assert!(lines.contains("SKY:LIKE:ok"), "{lines}");
}

#[test]
fn other_peoples_records_are_never_deleted() {
    let server = pds(|_| None);
    let rig = rig(&server);
    rig.core.send(Request::DeleteRecord {
        seq: 2,
        uri: "at://did:plc:someone-else/app.bsky.feed.post/3k".into(),
    });
    match rig.next() {
        Event::Failed { seq: 2, reason, .. } => assert!(reason.contains("not yours"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert!(server.to("deleteRecord").is_empty());
}

#[test]
fn a_reply_with_a_picture_uploads_first_and_names_root_and_parent() {
    let server = pds(|_| None);
    let rig = rig(&server);
    let file = picture("reply", "cat.png", 40, 30);
    let (root, parent) = (
        StrongRef {
            uri: "at://did:plc:r/app.bsky.feed.post/1".into(),
            cid: "bafyroot".into(),
        },
        subject(),
    );
    rig.core.send(Request::CreatePost {
        seq: 2,
        text: "nice cat".into(),
        reply: Some((root, parent)),
        pictures: vec![file],
    });
    assert!(matches!(rig.next(), Event::Created { seq: 2, .. }));
    let upload = &server.to("uploadBlob")[0];
    assert_eq!(upload.content_type, "image/png");
    assert!(upload.body.starts_with(b"\x89PNG"));
    let record = server.to("createRecord")[0].json();
    assert_eq!(record["collection"], "app.bsky.feed.post");
    let rec = &record["record"];
    assert_eq!(rec["text"], "nice cat");
    assert_eq!(rec["reply"]["root"]["cid"], "bafyroot");
    assert_eq!(rec["reply"]["parent"]["cid"], "bafypost");
    assert_eq!(rec["embed"]["$type"], "app.bsky.embed.images");
    assert_eq!(
        rec["embed"]["images"][0]["image"]["ref"]["$link"],
        "bafyblob"
    );
    assert_eq!(rec["embed"]["images"][0]["aspectRatio"]["width"], 40);
    assert_eq!(rec["embed"]["images"][0]["aspectRatio"]["height"], 30);
    // The upload came before the record.
    let order: Vec<_> = server
        .requests()
        .iter()
        .map(|s| s.path.rsplit('.').next().unwrap().to_string())
        .collect();
    let (up, rec_at) = (
        order.iter().position(|p| p == "uploadBlob").unwrap(),
        order.iter().position(|p| p == "createRecord").unwrap(),
    );
    assert!(up < rec_at);
}

#[test]
fn what_cannot_be_uploaded_stops_before_any_write() {
    let server = pds(|_| None);
    let rig = rig(&server);
    let dir = std::env::temp_dir().join(format!("lazysky-big-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let big = dir.join("big.png");
    let mut bytes = std::fs::read(picture("big", "small.png", 8, 8)).unwrap();
    bytes.resize(1_000_001, 0);
    std::fs::write(&big, bytes).unwrap();
    let text_file = dir.join("notes.png");
    std::fs::write(&text_file, "just text").unwrap();
    for (seq, file, expect) in [(2, big, "KB"), (3, text_file, "not a JPEG")] {
        rig.core.send(Request::CreatePost {
            seq,
            text: "x".into(),
            reply: None,
            pictures: vec![file],
        });
        match rig.next() {
            Event::Failed { seq: s, reason, .. } => {
                assert_eq!(s, seq);
                assert!(reason.contains(expect), "{reason}");
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(server.to("createRecord").is_empty());
    assert!(server.to("uploadBlob").is_empty());
}

#[test]
fn an_overlong_post_is_refused_by_the_client() {
    let server = pds(|_| None);
    let rig = rig(&server);
    rig.core.send(Request::CreatePost {
        seq: 2,
        text: "x".repeat(301),
        reply: None,
        pictures: vec![],
    });
    assert!(matches!(rig.next(), Event::Failed { seq: 2, .. }));
    assert!(server.to("createRecord").is_empty());
}

#[test]
fn threads_people_and_raw() {
    let server = pds(|seen| {
        if seen.path.contains("getPostThread") {
            return Some((
                200,
                json!({"thread": {"$type": "app.bsky.feed.defs#threadViewPost",
                    "post": post_view(1, "did:plc:a"), "replies": []}})
                .to_string(),
            ));
        }
        if seen.path.contains("getLikes") {
            return Some((
                200,
                json!({"cursor": "l2", "likes": [
                    {"actor": {"did": "did:plc:x", "handle": "x.test"}},
                    {"actor": {"did": "did:plc:y", "handle": "y.test", "displayName": "Y"}}]})
                .to_string(),
            ));
        }
        None
    });
    let rig = rig(&server);
    rig.core.send(Request::Thread {
        seq: 2,
        uri: "at://did:plc:a/app.bsky.feed.post/1".into(),
    });
    let Event::Thread { thread, .. } = rig.next() else {
        panic!("no thread")
    };
    assert_eq!(Thread::rows(&thread).len(), 1);
    assert!(server.to("getPostThread")[0]
        .path
        .contains("uri=at%3A%2F%2Fdid%3Aplc%3Aa"));

    rig.core.send(Request::Actors {
        seq: 3,
        list: ActorList::LikedBy("at://did:plc:a/app.bsky.feed.post/1".into()),
        cursor: None,
    });
    match rig.next() {
        Event::Actors { actors, cursor, .. } => {
            assert_eq!(actors.len(), 2);
            assert_eq!(cursor.as_deref(), Some("l2"));
        }
        other => panic!("{other:?}"),
    }

    rig.core.send(Request::Raw {
        seq: 4,
        json: Arc::new(json!({"a": [1, 2], "b": "x"})),
    });
    match rig.next() {
        Event::Raw { text, .. } => {
            assert!(text.contains("\n  \"a\": [\n    1,"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}
