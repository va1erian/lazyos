//! The mock Bluesky behind the window tests: one server that is the login
//! service and the PDS, with a paged timeline, pictures, threads, profiles,
//! bookmarks, notifications, search and the repo writes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use super::{serve, Mock, ME};

/// A PNG with a colour gradient and a diagonal stripe, tinted by `n`.
pub fn picture(w: u32, h: u32, n: u32) -> Vec<u8> {
    let img = image::RgbaImage::from_fn(w, h, |x, y| {
        if (x + y) % 64 < 8 {
            image::Rgba([255, 255, 255, 255])
        } else {
            image::Rgba([
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                (n.wrapping_mul(53) % 200) as u8,
                255,
            ])
        }
    });
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// The `postView` of post `n` (own when `n % 13 == 5`).
pub fn post_view(n: usize, base: &str) -> Value {
    let words = "the quick brown fox jumps over the lazy dog ";
    let text = format!("Post number {n}: {}", words.repeat(1 + n % 5).trim());
    let own = n % 13 == 5;
    let mut record = json!({"text": text, "createdAt": "2026-10-10T12:00:00Z"});
    if n.is_multiple_of(7) {
        record["facets"] = json!([{"index": {"byteStart": 0, "byteEnd": 4},
            "features": [{"$type": "app.bsky.richtext.facet#link",
                          "uri": format!("https://example.test/{n}")}]}]);
    }
    let mut view = json!({
        "uri": format!("at://{}/app.bsky.feed.post/{n}", if own { ME } else { "did:plc:a" }),
        "cid": format!("bafy{n}"),
        "author": {"did": if own { ME } else { "did:plc:a" },
                   "handle": if own { "me.test" } else { "alice.test" },
                   "displayName": if own { "Me" } else { "Alice" },
                   "avatar": format!("{base}/avatar.png")},
        "record": record, "likeCount": n % 40, "repostCount": n % 9,
        "replyCount": 0, "indexedAt": "2026-10-10T12:00:01Z"
    });
    match n % 6 {
        1 => {
            view["embed"] = json!({"$type": "app.bsky.embed.images#view", "images": [{
                "thumb": format!("{base}/img/{n}.png"),
                "fullsize": format!("{base}/img/{n}@png"),
                "aspectRatio": {"width": 4, "height": 3}}]});
        }
        3 => {
            view["embed"] = json!({"$type": "app.bsky.embed.record#view", "record": {
                "$type": "app.bsky.embed.record#viewRecord",
                "uri": "at://did:plc:q/app.bsky.feed.post/1", "cid": "c",
                "author": {"did": "did:plc:q", "handle": "q.test", "displayName": "Quentin"},
                "value": {"text": "an older thought worth quoting"}}});
        }
        _ => {}
    }
    view
}

/// The query value `key` of `path`, decoded.
fn query(path: &str, key: &str) -> Option<String> {
    let q = path.split_once('?')?.1;
    let raw = q
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")))?;
    let mut out = Vec::new();
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A mock Bluesky: one server is the login service and the PDS.
pub fn bluesky() -> Mock {
    let port = Arc::new(Mutex::new(0u16));
    let fail = Arc::new(AtomicBool::new(false));
    let (shared, fail_in) = (Arc::clone(&port), Arc::clone(&fail));
    let mut mock = serve(move |req| {
        let base = format!("http://127.0.0.1:{}", shared.lock().unwrap());
        let json = |status: u16, v: Value| (status, v.to_string().into_bytes());
        let path = req.path.as_str();
        if path.starts_with("/avatar.png") {
            return (200, picture(96, 96, 7));
        }
        if let Some(n) = path.strip_prefix("/img/") {
            // `/img/<n>.png` (a thumbnail) or `/img/<n>@png` (a full size).
            let n: u32 = n
                .split(['@', '.'])
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            return (200, picture(400, 300, n));
        }
        if path.contains("createSession") {
            return json(
                200,
                json!({"accessJwt": "a.b.c", "refreshJwt": "d.e.f", "did": ME,
                       "handle": "me.test",
                       "didDoc": {"service": [{"id": "#atproto_pds",
                                               "serviceEndpoint": base}]}}),
            );
        }
        if path.contains("createRecord") || path.contains("deleteRecord") {
            if fail_in.load(Ordering::SeqCst) {
                return json(
                    500,
                    json!({"error": "InternalServerError", "message": "the repo is down"}),
                );
            }
            if path.contains("deleteRecord") {
                return json(200, json!({}));
            }
            let body = req.json();
            let collection = body["collection"].as_str().unwrap_or("x");
            return json(
                200,
                json!({"uri": format!("at://{ME}/{collection}/3knew"), "cid": "bafynew"}),
            );
        }
        if path.contains("uploadBlob") {
            return json(
                200,
                json!({"blob": {"$type": "blob", "ref": {"$link": "bafyblob"},
                                "mimeType": "image/png", "size": req.body.len()}}),
            );
        }
        if path.contains("getPostThread") {
            let uri = query(path, "uri").unwrap_or_default();
            let n: usize = uri
                .rsplit('/')
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let reply = |k: usize| {
                json!({"$type": "app.bsky.feed.defs#threadViewPost",
                       "post": post_view(1000 + k, &base), "replies": []})
            };
            return json(
                200,
                json!({"thread": {"$type": "app.bsky.feed.defs#threadViewPost",
                    "post": post_view(n, &base),
                    "replies": [reply(1), reply(2)]}}),
            );
        }
        if path.contains("getLikes") {
            return json(
                200,
                json!({"likes": [
                    {"actor": {"did": "did:plc:x", "handle": "x.test", "displayName": "Xavier"}},
                    {"actor": {"did": "did:plc:y", "handle": "y.test"}}]}),
            );
        }
        if path.contains("getProfile") {
            return json(
                200,
                json!({"did": ME, "handle": "me.test", "displayName": "Me",
                       "description": "just me", "followersCount": 12,
                       "followsCount": 3, "postsCount": 40}),
            );
        }
        if path.contains("getAuthorFeed") {
            // The signed-in user's own media posts, one picture each.
            let feed: Vec<Value> = (600..605)
                .map(|n| {
                    let mut view = post_view(n, &base);
                    view["author"] = json!({"did": ME, "handle": "me.test",
                                            "displayName": "Me"});
                    view["embed"] = json!({"$type": "app.bsky.embed.images#view",
                        "images": [{"thumb": format!("{base}/img/{n}.png"),
                                    "fullsize": format!("{base}/img/{n}@png"),
                                    "aspectRatio": {"width": 4, "height": 3}}]});
                    json!({"post": view})
                })
                .collect();
            return json(200, json!({"feed": feed}));
        }
        if path.contains("getActorLikes") {
            let feed: Vec<Value> = (200..205)
                .map(|n| json!({"post": post_view(n, &base)}))
                .collect();
            return json(200, json!({"feed": feed}));
        }
        if path.contains("getBookmarks") {
            let bookmarks: Vec<Value> = (300..303)
                .map(|n| json!({"createdAt": "2026-10-10T00:00:00Z", "item": post_view(n, &base)}))
                .collect();
            return json(200, json!({"bookmarks": bookmarks}));
        }
        if path.contains("listNotifications") {
            return json(
                200,
                json!({"notifications": [
                    {"uri": "at://did:plc:x/app.bsky.feed.like/1", "cid": "c",
                     "author": {"did": "did:plc:x", "handle": "x.test", "displayName": "Xavier"},
                     "reason": "like", "reasonSubject": "at://did:plc:me/app.bsky.feed.post/5",
                     "record": {}, "isRead": false},
                    {"uri": "at://did:plc:y/app.bsky.feed.post/77", "cid": "c",
                     "author": {"did": "did:plc:y", "handle": "y.test"},
                     "reason": "reply", "record": {"text": "agreed!"}, "isRead": false}]}),
            );
        }
        if path.contains("getUnreadCount") {
            return json(200, json!({"count": 2}));
        }
        if path.contains("updateSeen") {
            return json(200, json!({}));
        }
        if path.contains("searchActors") {
            return json(
                200,
                json!({"actors": [
                    {"did": "did:plc:a", "handle": "alice.test", "displayName": "Alice"},
                    {"did": "did:plc:al", "handle": "alan.test"}]}),
            );
        }
        if path.contains("searchPosts") {
            let posts: Vec<Value> = (400..403).map(|n| post_view(n, &base)).collect();
            return json(200, json!({"posts": posts}));
        }
        if path.contains("getTimeline") {
            let page: usize = query(path, "cursor")
                .and_then(|c| c.strip_prefix("page-").and_then(|n| n.parse().ok()))
                .unwrap_or(0);
            let feed: Vec<Value> = (page * 30..page * 30 + 30)
                .map(|n| {
                    let mut entry = json!({"post": post_view(n, &base)});
                    if n.is_multiple_of(11) {
                        entry["reason"] = json!({
                            "$type": "app.bsky.feed.defs#reasonRepost",
                            "by": {"did": "did:plc:r", "handle": "r.test", "displayName": "Rita"}});
                    }
                    entry
                })
                .collect();
            let mut answer = json!({"feed": feed});
            if page + 1 < 10 {
                answer["cursor"] = json!(format!("page-{}", page + 1));
            }
            return json(200, answer);
        }
        json(404, json!({"error": "NotFound", "message": "nothing here"}))
    });
    *port.lock().unwrap() = mock.port;
    mock.fail_writes = fail;
    mock
}
