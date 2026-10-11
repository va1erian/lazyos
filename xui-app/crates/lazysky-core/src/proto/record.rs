//! Records the client writes (§7.3): likes, reposts, posts, and the
//! `createRecord`/`deleteRecord` request bodies around them.
//!
//! Builders are pure: the caller passes the timestamp.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::feed::StrongRef;

pub const LIKE: &str = "app.bsky.feed.like";
pub const REPOST: &str = "app.bsky.feed.repost";
pub const POST: &str = "app.bsky.feed.post";

/// The composer's limit. The lexicon counts graphemes; counting `char`s
/// over-counts combining sequences (an emoji with a modifier counts as two),
/// so this errs on the side of refusing, never of the server refusing.
pub const MAX_POST_CHARS: usize = 300;
pub const MAX_IMAGES: usize = 4;

/// An uploaded image to attach: the `blob` object `uploadBlob` returned.
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub blob: Value,
    pub alt: String,
    pub width: u32,
    pub height: u32,
}

pub fn like(subject: &StrongRef, created_at: &str) -> Value {
    json!({"$type": LIKE, "subject": subject.to_json(), "createdAt": created_at})
}

pub fn repost(subject: &StrongRef, created_at: &str) -> Value {
    json!({"$type": REPOST, "subject": subject.to_json(), "createdAt": created_at})
}

/// A new post, optionally a reply (`root`, `parent`) and with up to four
/// images.
pub fn post(
    text: &str,
    created_at: &str,
    reply: Option<(&StrongRef, &StrongRef)>,
    images: &[Attachment],
) -> Result<Value, String> {
    if text.trim().is_empty() && images.is_empty() {
        return Err("a post needs text or a picture".into());
    }
    let length = text.chars().count();
    if length > MAX_POST_CHARS {
        return Err(format!(
            "the post is {length} characters; the limit is {MAX_POST_CHARS}"
        ));
    }
    if images.len() > MAX_IMAGES {
        return Err(format!("a post takes at most {MAX_IMAGES} pictures"));
    }
    let mut record = json!({
        "$type": POST,
        "text": text,
        "createdAt": created_at,
        "langs": ["en"],
    });
    if let Some((root, parent)) = reply {
        record["reply"] = json!({"root": root.to_json(), "parent": parent.to_json()});
    }
    if !images.is_empty() {
        let list: Vec<Value> = images
            .iter()
            .map(|a| {
                json!({"image": a.blob, "alt": a.alt,
                       "aspectRatio": {"width": a.width.max(1), "height": a.height.max(1)}})
            })
            .collect();
        record["embed"] = json!({"$type": "app.bsky.embed.images", "images": list});
    }
    Ok(record)
}

/// The body of `com.atproto.repo.createRecord`.
pub fn create_record(repo_did: &str, collection: &str, record: Value) -> Value {
    json!({"repo": repo_did, "collection": collection, "record": record})
}

/// The body of `com.atproto.repo.deleteRecord`.
pub fn delete_record(repo_did: &str, collection: &str, rkey: &str) -> Value {
    json!({"repo": repo_did, "collection": collection, "rkey": rkey})
}

/// `(collection, rkey)` of an `at://did/collection/rkey` URI.
pub fn parts_of(uri: &str) -> Option<(&str, &str)> {
    let rest = uri.strip_prefix("at://")?;
    let mut parts = rest.splitn(3, '/');
    let _did = parts.next().filter(|s| !s.is_empty())?;
    let collection = parts.next().filter(|s| !s.is_empty())?;
    let rkey = parts.next().filter(|s| !s.is_empty() && !s.contains('/'))?;
    Some((collection, rkey))
}

/// The MIME type of an image from its first bytes (`uploadBlob` takes the
/// bytes and a `Content-Type`; a file's name proves nothing).
pub fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// `1970-01-01T00:00:00.000Z` for `unix_secs` (UTC, proleptic Gregorian).
pub fn rfc3339_utc(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs = unix_secs % 86_400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// The current time as `createdAt` wants it.
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    rfc3339_utc(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sref(n: u32) -> StrongRef {
        StrongRef {
            uri: format!("at://did:plc:a/app.bsky.feed.post/{n}"),
            cid: format!("bafy{n}"),
        }
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00.000Z");
        assert_eq!(rfc3339_utc(1_791_711_296), "2026-10-11T09:34:56.000Z");
    }

    #[test]
    fn likes_and_reposts_pin_the_subject() {
        let r = like(&sref(1), "2026-01-01T00:00:00.000Z");
        assert_eq!(r["$type"], LIKE);
        assert_eq!(r["subject"]["cid"], "bafy1");
        let body = create_record("did:plc:me", LIKE, r);
        assert_eq!(body["repo"], "did:plc:me");
        assert_eq!(body["collection"], LIKE);
    }

    #[test]
    fn a_reply_names_root_and_parent() {
        let (root, parent) = (sref(1), sref(2));
        let r = post("hi", "t", Some((&root, &parent)), &[]).unwrap();
        assert_eq!(r["reply"]["root"]["uri"], root.uri);
        assert_eq!(r["reply"]["parent"]["uri"], parent.uri);
        assert_eq!(r["langs"][0], "en");
        assert!(r.get("embed").is_none());
    }

    #[test]
    fn images_attach_with_alt_and_aspect() {
        let a = Attachment {
            blob: json!({"$type": "blob", "mimeType": "image/png", "size": 10}),
            alt: "a dog".into(),
            width: 800,
            height: 600,
        };
        let r = post("", "t", None, &[a]).unwrap();
        assert_eq!(r["embed"]["$type"], "app.bsky.embed.images");
        assert_eq!(r["embed"]["images"][0]["alt"], "a dog");
        assert_eq!(r["embed"]["images"][0]["aspectRatio"]["width"], 800);
    }

    #[test]
    fn limits_are_enforced_before_the_network() {
        assert!(post("", "t", None, &[]).is_err());
        assert!(post("   ", "t", None, &[]).is_err());
        assert!(post(&"x".repeat(301), "t", None, &[])
            .unwrap_err()
            .contains("301"));
        assert!(post(&"x".repeat(300), "t", None, &[]).is_ok());
        let a = Attachment {
            blob: json!({}),
            alt: String::new(),
            width: 1,
            height: 1,
        };
        assert!(post("x", "t", None, &vec![a; 5]).is_err());
    }

    #[test]
    fn at_uris_split_into_collection_and_rkey() {
        assert_eq!(
            parts_of("at://did:plc:a/app.bsky.feed.like/3kabc"),
            Some((LIKE, "3kabc"))
        );
        assert_eq!(parts_of("at://did:plc:a/app.bsky.feed.like"), None);
        assert_eq!(parts_of("https://x/y/z"), None);
        assert_eq!(parts_of("at://did:plc:a/c/r/extra"), None);
    }

    #[test]
    fn image_types_come_from_magic_bytes() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff_image(b"GIF89a.."), Some("image/gif"));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_image(b"<svg>"), None);
        assert_eq!(sniff_image(b""), None);
    }
}
