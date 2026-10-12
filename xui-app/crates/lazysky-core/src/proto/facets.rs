//! Rich-text facets (§7.3): links, mentions and tags over **UTF-8 byte
//! offsets** of the post text.
//!
//! A facet is believed only if `start < end <= len` and both offsets fall on
//! `char` boundaries; one that fails is dropped, never clamped. Link targets
//! other than `http:`/`https:` are dropped (a post must not make the client
//! open `javascript:` or `file:`), and overlapping facets keep the first.

use serde_json::Value;

use super::{text, type_of};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feature {
    Link(String),
    /// A mention, by DID.
    Mention(String),
    Tag(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facet {
    /// Byte offsets into the post text.
    pub start: usize,
    pub end: usize,
    pub feature: Feature,
}

/// A run of the text, with the feature that covers it (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    pub text: &'a str,
    pub feature: Option<&'a Feature>,
}

/// The valid facets of `record["facets"]` for `body`, sorted by position.
pub fn parse(body: &str, record: &Value) -> Vec<Facet> {
    let Some(list) = record.get("facets").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut facets: Vec<Facet> = list.iter().filter_map(|f| one(body, f)).collect();
    facets.sort_by_key(|f| (f.start, f.end));
    let mut kept: Vec<Facet> = Vec::with_capacity(facets.len());
    for facet in facets {
        if kept.last().is_none_or(|last| facet.start >= last.end) {
            kept.push(facet);
        }
    }
    kept
}

fn one(body: &str, v: &Value) -> Option<Facet> {
    let index = v.get("index")?;
    let start = usize::try_from(index.get("byteStart")?.as_u64()?).ok()?;
    let end = usize::try_from(index.get("byteEnd")?.as_u64()?).ok()?;
    if start >= end || end > body.len() {
        return None;
    }
    if !body.is_char_boundary(start) || !body.is_char_boundary(end) {
        return None;
    }
    let feature = v.get("features")?.as_array()?.iter().find_map(feature_of)?;
    Some(Facet {
        start,
        end,
        feature,
    })
}

fn feature_of(v: &Value) -> Option<Feature> {
    match type_of(v) {
        "app.bsky.richtext.facet#link" => {
            let uri = text(v, "uri")?;
            let lower = uri.to_ascii_lowercase();
            (lower.starts_with("https://") || lower.starts_with("http://"))
                .then_some(Feature::Link(uri))
        }
        "app.bsky.richtext.facet#mention" => text(v, "did").map(Feature::Mention),
        "app.bsky.richtext.facet#tag" => text(v, "tag").map(Feature::Tag),
        _ => None,
    }
}

/// Splits `body` at the facets' edges. The segments cover the whole text.
pub fn segments<'a>(body: &'a str, facets: &'a [Facet]) -> Vec<Segment<'a>> {
    let mut out = Vec::with_capacity(facets.len() * 2 + 1);
    let mut at = 0;
    for facet in facets {
        // `parse` guarantees order and bounds; stay safe for hand-made input.
        if facet.start < at
            || facet.end > body.len()
            || !body.is_char_boundary(facet.start)
            || !body.is_char_boundary(facet.end)
        {
            continue;
        }
        if facet.start > at {
            out.push(Segment {
                text: &body[at..facet.start],
                feature: None,
            });
        }
        out.push(Segment {
            text: &body[facet.start..facet.end],
            feature: Some(&facet.feature),
        });
        at = facet.end;
    }
    if at < body.len() {
        out.push(Segment {
            text: &body[at..],
            feature: None,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(start: u64, end: u64, uri: &str) -> Value {
        json!({"index": {"byteStart": start, "byteEnd": end},
               "features": [{"$type": "app.bsky.richtext.facet#link", "uri": uri}]})
    }

    #[test]
    fn offsets_are_utf8_bytes_not_chars() {
        // "é" is two bytes: the link covers "site" at bytes 3..7.
        let body = "é: site";
        assert_eq!(&body[4..8], "site");
        let record = json!({"facets": [link(4, 8, "https://site.test")]});
        let facets = parse(body, &record);
        assert_eq!(facets.len(), 1);
        let parts = segments(body, &facets);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1].text, "site");
    }

    #[test]
    fn bad_offsets_are_dropped_not_clamped() {
        let body = "héllo";
        let record = json!({"facets": [
            link(2, 3, "https://a.test"),   // inside the two-byte é
            link(0, 99, "https://b.test"),  // past the end
            link(3, 3, "https://c.test"),   // empty
            link(4, 2, "https://d.test"),   // backwards
        ]});
        assert!(parse(body, &record).is_empty());
    }

    #[test]
    fn only_web_links_are_believed() {
        let body = "click here";
        let record = json!({"facets": [link(0, 5, "javascript:alert(1)")]});
        assert!(parse(body, &record).is_empty());
        let record = json!({"facets": [link(0, 5, "file:///etc/passwd")]});
        assert!(parse(body, &record).is_empty());
    }

    #[test]
    fn overlaps_keep_the_first() {
        let body = "0123456789";
        let record =
            json!({"facets": [link(5, 9, "https://b.test"), link(0, 6, "https://a.test")]});
        let facets = parse(body, &record);
        assert_eq!(facets.len(), 1);
        assert_eq!(facets[0].feature, Feature::Link("https://a.test".into()));
    }

    #[test]
    fn mentions_and_tags_parse() {
        let body = "@alice #rust";
        let record = json!({"facets": [
            {"index": {"byteStart": 0, "byteEnd": 6},
             "features": [{"$type": "app.bsky.richtext.facet#mention", "did": "did:plc:a"}]},
            {"index": {"byteStart": 7, "byteEnd": 12},
             "features": [{"$type": "app.bsky.richtext.facet#tag", "tag": "rust"}]},
        ]});
        let facets = parse(body, &record);
        assert_eq!(facets[0].feature, Feature::Mention("did:plc:a".into()));
        assert_eq!(facets[1].feature, Feature::Tag("rust".into()));
        let joined: String = segments(body, &facets).iter().map(|s| s.text).collect();
        assert_eq!(joined, body);
    }

    #[test]
    fn facets_of_the_wrong_shape_are_ignored() {
        for record in [
            json!({}),
            json!({"facets": 3}),
            json!({"facets": [1, "x", null]}),
        ] {
            assert!(parse("text", &record).is_empty());
        }
    }
}
