//! Post embeds (§7.3): images, an external card, a quoted post, a quote with
//! media, and a video (a thumbnail and a badge; v1 never plays it).
//!
//! An unknown `$type` is [`Embed::Unknown`] and never fails a page. Quotes
//! nest at most [`MAX_DEPTH`] deep, so hostile JSON cannot recurse far.

use serde_json::Value;

use super::actors::{web_url, Actor};
use super::facets::{self, Facet};
use super::{text, type_of};

/// How deep quotes inside quotes are followed.
pub const MAX_DEPTH: u8 = 3;
/// The aspect ratio a missing or invalid `aspectRatio` is replaced by.
pub const DEFAULT_ASPECT: (u32, u32) = (16, 9);
/// A post's text is cut here (the lexicon allows 3000 bytes; the composer 300
/// characters).
pub const MAX_TEXT_BYTES: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub thumb: String,
    pub fullsize: String,
    pub alt: String,
    /// `(width, height)` as the API gives it, when valid.
    pub aspect: Option<(u32, u32)>,
}

impl ImageRef {
    /// The ratio used to size the image's box before it loads (R2).
    pub fn aspect_or_default(&self) -> (u32, u32) {
        self.aspect.unwrap_or(DEFAULT_ASPECT)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalCard {
    pub uri: String,
    pub title: String,
    pub description: String,
    pub thumb: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoRef {
    pub thumbnail: Option<String>,
    pub alt: String,
    pub aspect: Option<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotedPost {
    pub uri: String,
    pub cid: String,
    pub author: Actor,
    pub text: String,
    pub facets: Vec<Facet>,
    pub created_at: String,
    pub embed: Embed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Quoted {
    Post(Box<QuotedPost>),
    NotFound(String),
    Blocked(String),
    Detached(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Embed {
    #[default]
    None,
    Images(Vec<ImageRef>),
    External(ExternalCard),
    Record(Quoted),
    RecordWithMedia(Quoted, Box<Embed>),
    Video(VideoRef),
    Unknown,
}

impl Embed {
    /// Parses an embed *view* (the `embed` of a `postView`).
    pub fn parse(v: &Value) -> Embed {
        parse(v, 0)
    }

    /// Every image the embed shows, the media of a quote included.
    pub fn images(&self) -> &[ImageRef] {
        match self {
            Embed::Images(images) => images,
            Embed::RecordWithMedia(_, media) => media.images(),
            _ => &[],
        }
    }
}

pub(crate) fn parse(v: &Value, depth: u8) -> Embed {
    if !v.is_object() {
        return Embed::None;
    }
    match type_of(v) {
        "app.bsky.embed.images#view" => Embed::Images(images(v)),
        "app.bsky.embed.external#view" => v
            .get("external")
            .and_then(external)
            .map_or(Embed::Unknown, Embed::External),
        "app.bsky.embed.video#view" => Embed::Video(VideoRef {
            thumbnail: text(v, "thumbnail").filter(|u| web_url(u)),
            alt: text(v, "alt").unwrap_or_default(),
            aspect: aspect(v),
        }),
        "app.bsky.embed.record#view" => match v.get("record") {
            Some(record) => Embed::Record(quoted(record, depth)),
            None => Embed::Unknown,
        },
        "app.bsky.embed.recordWithMedia#view" => {
            let record = v.get("record").and_then(|r| r.get("record"));
            let media = v.get("media").map_or(Embed::None, |m| parse(m, depth));
            match record {
                Some(record) => Embed::RecordWithMedia(quoted(record, depth), Box::new(media)),
                None => media,
            }
        }
        _ => Embed::Unknown,
    }
}

fn images(v: &Value) -> Vec<ImageRef> {
    let Some(list) = v.get("images").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|image| {
            let thumb = text(image, "thumb").filter(|u| web_url(u))?;
            let fullsize = text(image, "fullsize")
                .filter(|u| web_url(u))
                .unwrap_or_else(|| thumb.clone());
            Some(ImageRef {
                thumb,
                fullsize,
                alt: text(image, "alt").unwrap_or_default(),
                aspect: aspect(image),
            })
        })
        .take(4)
        .collect()
}

/// `aspectRatio: {width, height}`, when both are sane.
fn aspect(v: &Value) -> Option<(u32, u32)> {
    let ratio = v.get("aspectRatio")?;
    let w = u32::try_from(ratio.get("width")?.as_u64()?).ok()?;
    let h = u32::try_from(ratio.get("height")?.as_u64()?).ok()?;
    (1..=100_000).contains(&w).then_some(())?;
    (1..=100_000).contains(&h).then_some((w, h))
}

fn external(v: &Value) -> Option<ExternalCard> {
    let uri = text(v, "uri").filter(|u| web_url(u))?;
    Some(ExternalCard {
        title: text(v, "title").unwrap_or_default(),
        description: text(v, "description").unwrap_or_default(),
        thumb: text(v, "thumb").filter(|u| web_url(u)),
        uri,
    })
}

fn quoted(v: &Value, depth: u8) -> Quoted {
    let uri = text(v, "uri").unwrap_or_default();
    match type_of(v) {
        "app.bsky.embed.record#viewNotFound" => Quoted::NotFound(uri),
        "app.bsky.embed.record#viewBlocked" => Quoted::Blocked(uri),
        "app.bsky.embed.record#viewDetached" => Quoted::Detached(uri),
        "app.bsky.embed.record#viewRecord" => {
            viewed(v, depth).map_or(Quoted::Unknown, |post| Quoted::Post(Box::new(post)))
        }
        _ => Quoted::Unknown,
    }
}

fn viewed(v: &Value, depth: u8) -> Option<QuotedPost> {
    let author = Actor::from_json(v.get("author")?)?;
    let value = v.get("value")?;
    let mut body = text(value, "text").unwrap_or_default();
    truncate(&mut body, MAX_TEXT_BYTES);
    let nested = if depth + 1 >= MAX_DEPTH {
        Embed::None
    } else {
        v.get("embeds")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .map_or(Embed::None, |e| parse(e, depth + 1))
    };
    Some(QuotedPost {
        uri: text(v, "uri")?,
        cid: text(v, "cid").unwrap_or_default(),
        author,
        facets: facets::parse(&body, value),
        created_at: text(value, "createdAt")
            .or_else(|| text(v, "indexedAt"))
            .unwrap_or_default(),
        text: body,
        embed: nested,
    })
}

/// Cuts `s` to at most `max` bytes on a `char` boundary.
pub(crate) fn truncate(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn author() -> Value {
        json!({"did": "did:plc:q", "handle": "q.test"})
    }

    #[test]
    fn images_keep_urls_alt_and_a_valid_aspect() {
        let v = json!({"$type": "app.bsky.embed.images#view", "images": [
            {"thumb": "https://cdn.test/t1", "fullsize": "https://cdn.test/f1", "alt": "a cat",
             "aspectRatio": {"width": 2000, "height": 1000}},
            {"thumb": "https://cdn.test/t2", "fullsize": "https://cdn.test/f2", "alt": ""},
            {"thumb": "ftp://cdn.test/t3", "fullsize": "x", "alt": ""},
            {"thumb": "https://cdn.test/t4", "fullsize": "https://cdn.test/f4",
             "aspectRatio": {"width": 0, "height": 5}},
        ]});
        let embed = Embed::parse(&v);
        let images = embed.images();
        assert_eq!(images.len(), 3, "the ftp image is dropped");
        assert_eq!(images[0].aspect, Some((2000, 1000)));
        assert_eq!(images[1].aspect_or_default(), DEFAULT_ASPECT);
        assert_eq!(images[2].aspect, None, "a zero width is no aspect");
    }

    #[test]
    fn more_than_four_images_are_cut() {
        let one = json!({"thumb": "https://cdn.test/t", "fullsize": "https://cdn.test/f"});
        let v = json!({"$type": "app.bsky.embed.images#view", "images": [one, one, one, one, one, one]});
        assert_eq!(Embed::parse(&v).images().len(), 4);
    }

    #[test]
    fn an_unknown_type_is_unknown_not_an_error() {
        assert_eq!(
            Embed::parse(&json!({"$type": "app.example.hologram#view"})),
            Embed::Unknown
        );
        assert_eq!(Embed::parse(&json!(null)), Embed::None);
        assert_eq!(Embed::parse(&json!("text")), Embed::None);
    }

    #[test]
    fn a_quote_with_media_gives_both() {
        let v = json!({"$type": "app.bsky.embed.recordWithMedia#view",
            "record": {"record": {"$type": "app.bsky.embed.record#viewRecord",
                "uri": "at://did:plc:q/app.bsky.feed.post/3k", "cid": "bafy", "author": author(),
                "value": {"text": "quoted words", "createdAt": "2026-01-01T00:00:00Z"}}},
            "media": {"$type": "app.bsky.embed.images#view", "images": [
                {"thumb": "https://cdn.test/t", "fullsize": "https://cdn.test/f"}]}});
        match Embed::parse(&v) {
            Embed::RecordWithMedia(Quoted::Post(p), media) => {
                assert_eq!(p.text, "quoted words");
                assert_eq!(media.images().len(), 1);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn missing_and_blocked_quotes_are_named() {
        let blocked = json!({"$type": "app.bsky.embed.record#view", "record":
            {"$type": "app.bsky.embed.record#viewBlocked", "uri": "at://x/y/z"}});
        assert_eq!(
            Embed::parse(&blocked),
            Embed::Record(Quoted::Blocked("at://x/y/z".into()))
        );
        let weird =
            json!({"$type": "app.bsky.embed.record#view", "record": {"$type": "app.new#thing"}});
        assert_eq!(Embed::parse(&weird), Embed::Record(Quoted::Unknown));
    }

    #[test]
    fn quotes_nest_to_a_limit() {
        // Build a quote of a quote of a quote ... 8 deep.
        let mut inner = json!({"$type": "app.bsky.embed.record#viewRecord",
            "uri": "at://d/p/0", "author": author(), "value": {"text": "leaf"}});
        for n in 1..8 {
            inner = json!({"$type": "app.bsky.embed.record#viewRecord",
                "uri": format!("at://d/p/{n}"), "author": author(), "value": {"text": "mid"},
                "embeds": [{"$type": "app.bsky.embed.record#view", "record": inner}]});
        }
        let v = json!({"$type": "app.bsky.embed.record#view", "record": inner});
        let mut depth = 0;
        let mut cur = Embed::parse(&v);
        while let Embed::Record(Quoted::Post(p)) = cur {
            depth += 1;
            cur = p.embed;
        }
        assert_eq!(depth, MAX_DEPTH as usize);
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let mut s = "é".repeat(10);
        truncate(&mut s, 5);
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn video_and_external() {
        let v = json!({"$type": "app.bsky.embed.video#view", "thumbnail": "https://cdn.test/v.jpg",
            "aspectRatio": {"width": 1080, "height": 1920}});
        assert!(matches!(
            Embed::parse(&v),
            Embed::Video(VideoRef {
                aspect: Some((1080, 1920)),
                ..
            })
        ));
        let e = json!({"$type": "app.bsky.embed.external#view", "external":
            {"uri": "https://example.test/a", "title": "A", "description": "d"}});
        assert!(matches!(Embed::parse(&e), Embed::External(c) if c.title == "A"));
        let bad = json!({"$type": "app.bsky.embed.external#view", "external": {"uri": "data:text/html,x"}});
        assert_eq!(Embed::parse(&bad), Embed::Unknown);
    }
}
