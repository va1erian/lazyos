//! Writes through the api thread: likes and undoing them, deletes (own
//! records only), a reply with a picture, the picture limits, threads and
//! the Raw view.

mod common;

use std::sync::Arc;

use common::*;
use lazysky_core::proto::{StrongRef, Thread};
use lazysky_core::ActorList;
use lazysky_core::{Event, Request};
use serde_json::json;

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
