//! Writing: likes, reposts, deletes and new posts with pictures.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::ImageReader;

use super::failed;
use crate::net::api::{Api, ApiError, Notes};
use crate::proto::record::{self, Attachment};
use crate::proto::StrongRef;
use crate::{Event, Platform};

/// Bluesky's limit for one uploaded picture.
pub const MAX_PICTURE_BYTES: usize = 1_000_000;

const CREATE: &str = "com.atproto.repo.createRecord";
const DELETE: &str = "com.atproto.repo.deleteRecord";
const UPLOAD: &str = "com.atproto.repo.uploadBlob";

fn created(
    api: &mut Api,
    notes: &dyn Notes,
    collection: &str,
    record: serde_json::Value,
) -> Result<(String, String), ApiError> {
    let did = api.did().ok_or(ApiError::NotSignedIn)?.to_string();
    let body = record::create_record(&did, collection, record);
    let v = api.post(CREATE, &body, notes)?;
    let field = |name: &str| {
        v.get(name)
            .and_then(|x| x.as_str())
            .map(str::to_owned)
            .ok_or_else(|| ApiError::Transport(format!("the server's answer has no {name}")))
    };
    Ok((field("uri")?, field("cid")?))
}

/// A like or a repost of `subject`.
pub(super) fn react(
    api: &mut Api,
    platform: &dyn Platform,
    notes: &dyn Notes,
    seq: u64,
    collection: &str,
    subject: &StrongRef,
) -> Event {
    let at = record::now_rfc3339();
    let record = if collection == record::LIKE {
        record::like(subject, &at)
    } else {
        record::repost(subject, &at)
    };
    match created(api, notes, collection, record) {
        Ok((uri, cid)) => {
            let what = if collection == record::LIKE {
                "LIKE"
            } else {
                "REPOST"
            };
            platform.log(&format!("SKY:{what}:ok"));
            Event::Created { seq, uri, cid }
        }
        Err(error) => {
            let what = if collection == record::LIKE {
                "LIKE"
            } else {
                "REPOST"
            };
            platform.log(&format!("SKY:{what}:fail"));
            failed(seq, &error)
        }
    }
}

/// Deletes one of the signed-in user's own records.
pub(super) fn delete(
    api: &mut Api,
    platform: &dyn Platform,
    notes: &dyn Notes,
    seq: u64,
    uri: &str,
) -> Event {
    let Some((collection, rkey)) = record::parts_of(uri) else {
        return failed(seq, &ApiError::Transport("not a record address".into()));
    };
    let Some(did) = api.did().map(str::to_owned) else {
        return failed(seq, &ApiError::NotSignedIn);
    };
    // Only the signed-in user's own records: the server would refuse, but
    // the client does not even ask.
    let owner = uri
        .strip_prefix("at://")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("");
    if owner != did {
        return failed(seq, &ApiError::Transport("that record is not yours".into()));
    }
    let body = record::delete_record(&did, collection, rkey);
    match api.post(DELETE, &body, notes) {
        Ok(_) => {
            platform.log(&format!("SKY:DELETE:ok:{collection}"));
            Event::Deleted {
                seq,
                uri: uri.to_string(),
            }
        }
        Err(error) => failed(seq, &error),
    }
}

/// Reads and uploads one picture file.
fn upload(api: &mut Api, notes: &dyn Notes, path: &Path) -> Result<Attachment, ApiError> {
    let bad = |why: String| ApiError::Transport(format!("{}: {why}", path.display()));
    let bytes = std::fs::read(path).map_err(|e| bad(e.to_string()))?;
    if bytes.len() > MAX_PICTURE_BYTES {
        return Err(bad(format!(
            "the picture is {} KB; Bluesky takes up to {} KB",
            bytes.len() / 1000,
            MAX_PICTURE_BYTES / 1000
        )));
    }
    let mime =
        record::sniff_image(&bytes).ok_or_else(|| bad("not a JPEG, PNG, GIF or WebP".into()))?;
    let (width, height) = ImageReader::new(Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok())
        .unwrap_or((1, 1));
    let answer = api.post_bytes(UPLOAD, mime, bytes, notes)?;
    let blob = answer
        .get("blob")
        .cloned()
        .ok_or_else(|| bad("the server's answer has no blob".into()))?;
    Ok(Attachment {
        blob,
        alt: String::new(),
        width,
        height,
    })
}

/// A new post: upload the pictures, then write the record.
pub(super) fn post(
    api: &mut Api,
    platform: &dyn Platform,
    notes: &dyn Notes,
    seq: u64,
    text: &str,
    reply: Option<(StrongRef, StrongRef)>,
    pictures: &[PathBuf],
) -> Event {
    let mut attachments = Vec::new();
    for path in pictures.iter().take(record::MAX_IMAGES) {
        match upload(api, notes, path) {
            Ok(a) => attachments.push(a),
            Err(error) => return failed(seq, &error),
        }
    }
    let reply_refs = reply.as_ref().map(|(root, parent)| (root, parent));
    let record = match record::post(text, &record::now_rfc3339(), reply_refs, &attachments) {
        Ok(record) => record,
        Err(why) => return failed(seq, &ApiError::Transport(why)),
    };
    match created(api, notes, record::POST, record) {
        Ok((uri, cid)) => {
            platform.log(&format!("SKY:POST:PASS:{uri}"));
            Event::Created { seq, uri, cid }
        }
        Err(error) => failed(seq, &error),
    }
}

/// `updateSeen`: everything up to now has been seen.
pub(super) fn mark_seen(api: &mut Api, notes: &dyn Notes) -> Result<(), ApiError> {
    let body = serde_json::json!({"seenAt": record::now_rfc3339()});
    api.post("app.bsky.notification.updateSeen", &body, notes)?;
    Ok(())
}
