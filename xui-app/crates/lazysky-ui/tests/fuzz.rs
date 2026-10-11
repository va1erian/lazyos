//! Hostile and odd post text through the layout path: a panic anywhere in
//! `prepare` would abort the whole app (`panic = "abort"`), so random texts
//! with emoji, CRLF, tabs, RTL, combining marks, control characters, huge
//! words and facets over every kind of boundary must all lay out.

use lazysky_core::proto::{FeedItem, Post};
use lazysky_ui::prepare::{prepare, Context, DEFAULT_MAX_IMAGE_H};
use serde_json::json;
use xui_canvas::OffscreenBackend;
use xui_core::backend::Backend;

const PIECES: &[&str] = &[
    "plain words ", "a", " ", "\n", "\r\n", "\t", "\u{1F980}", "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
    "\u{5E9}\u{5DC}\u{5D5}\u{5DD}", "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64A}\u{629}", "e\u{301}\u{302}",
    "\u{200B}", "\u{202E}", "\u{0}", "\u{7}", "\u{FEFF}", "日本語のテキスト", "https://example.test/a/very/long/path?x=1&y=2",
    "@someone.bsky.social", "#tag", "ΑΒΓ", "\u{10FFFF}", "\u{FFFD}",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self, below: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % below.max(1)
    }
}

fn text(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..rng.next(40) {
        s.push_str(PIECES[rng.next(PIECES.len())]);
    }
    if rng.next(6) == 0 {
        s.push_str(&"x".repeat(rng.next(3000)));
    }
    s
}

/// Facets over random char boundaries (some invalid ones, which parsing drops).
fn facets(rng: &mut Rng, s: &str) -> serde_json::Value {
    let bounds: Vec<usize> = s.char_indices().map(|(i, _)| i).chain([s.len()]).collect();
    let list: Vec<_> = (0..rng.next(6))
        .map(|_| {
            let a = bounds[rng.next(bounds.len())];
            let b = bounds[rng.next(bounds.len())];
            let (start, end) = (a.min(b), a.max(b));
            json!({"index": {"byteStart": start, "byteEnd": end},
                   "features": [{"$type": "app.bsky.richtext.facet#link", "uri": "https://e.test/"}]})
        })
        .collect();
    json!(list)
}

#[test]
fn random_texts_lay_out_without_panicking() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
    let shaper = OffscreenBackend::new().text_shaper();
    let mut rng = Rng(0xD15EA5E);
    for round in 0..600 {
        let body = text(&mut rng);
        let name = text(&mut rng);
        let quoted = text(&mut rng);
        let mut view = json!({
            "uri": format!("at://did:plc:a/app.bsky.feed.post/{round}"), "cid": "c",
            "author": {"did": "did:plc:a", "handle": "a.test", "displayName": name},
            "record": {"text": body, "createdAt": "2026-10-10T12:00:00Z"},
            "embed": {"$type": "app.bsky.embed.record#view", "record": {
                "$type": "app.bsky.embed.record#viewRecord", "uri": "at://d/p/1", "cid": "c",
                "author": {"did": "did:plc:q", "handle": "q.test", "displayName": text(&mut rng)},
                "value": {"text": quoted}}}
        });
        view["record"]["facets"] = facets(&mut rng, &body);
        let Some(post) = Post::from_view(&view) else {
            continue;
        };
        for width in [120, 333, 800, 2000] {
            let ctx = Context {
                width,
                dpi: if round % 2 == 0 { 96 } else { 192 },
                now: 1_791_711_296,
                max_image_h: DEFAULT_MAX_IMAGE_H,
            };
            let item = FeedItem {
                post: post.clone(),
                reason: None,
            };
            let prepared = prepare(&*shaper, &item, ctx);
            assert!(prepared.height > 0);
        }
    }
}
