//! The jank budget's paint bench (docs/lazysky-plan.md R7):
//!
//! ```text
//! cargo test -p lazysky-ui --release bench -- --ignored --nocapture
//! ```
//!
//! Paints 1,000 frames of a 2,000-row feed at 1280x720 while scrolling and
//! fails when the 99th percentile exceeds 4 ms (host budget).

use std::sync::Arc;
use std::time::Instant;

use lazysky_core::feed::FeedRow;
use lazysky_core::proto::{FeedItem, Post};
use lazysky_ui::feedview::paint::paint;
use lazysky_ui::feedview::ViewState;
use lazysky_ui::prepare::{prepare, Context, DEFAULT_MAX_IMAGE_H};
use serde_json::json;
use xui_canvas::{OffscreenBackend, Surface};
use xui_core::backend::Backend;
use xui_core::geometry::Rect;
use xui_core::Theme;

const ROWS: usize = 2000;
const FRAMES: usize = 1000;

fn item(n: usize) -> FeedItem {
    let words = "the quick brown fox jumps over the lazy dog ";
    let mut view = json!({
        "uri": format!("at://did:plc:a/app.bsky.feed.post/{n}"), "cid": "c",
        "author": {"did": "did:plc:a", "handle": "alice.test", "displayName": "Alice"},
        "record": {"text": format!("Post {n}: {}", words.repeat(1 + n % 6)),
                   "createdAt": "2026-10-10T12:00:00Z"},
        "likeCount": n % 30, "repostCount": n % 7
    });
    match n % 5 {
        1 => {
            view["embed"] = json!({"$type": "app.bsky.embed.images#view", "images": [
                {"thumb": "https://cdn.test/t", "fullsize": "https://cdn.test/f",
                 "aspectRatio": {"width": 4, "height": 3}},
                {"thumb": "https://cdn.test/t", "fullsize": "https://cdn.test/f"}]});
        }
        3 => {
            view["embed"] = json!({"$type": "app.bsky.embed.record#view", "record": {
                "$type": "app.bsky.embed.record#viewRecord", "uri": "at://d/p/1", "cid": "c",
                "author": {"did": "did:plc:q", "handle": "q.test"},
                "value": {"text": "an older thought worth quoting"}}});
        }
        _ => {}
    }
    FeedItem {
        post: Post::from_view(&view).unwrap(),
        reason: None,
    }
}

#[test]
#[ignore = "release-mode bench: cargo test -p lazysky-ui --release bench -- --ignored"]
fn bench_paint_a_two_thousand_row_feed() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
    let shaper = OffscreenBackend::new().text_shaper();
    let (width, height) = (1280, 720);
    let mut state = ViewState::new();
    state.set_view(width, height, 96);
    let ctx = Context {
        width: state.content_width(),
        dpi: 96,
        now: 1_791_711_296,
        max_image_h: DEFAULT_MAX_IMAGE_H,
    };
    let prepared_at = Instant::now();
    let rows: Vec<FeedRow<_>> = (0..ROWS)
        .map(|n| {
            let item = Arc::new(item(n));
            let prepared = prepare(&*shaper, &item, ctx);
            FeedRow::post(item, prepared.height, Some(prepared))
        })
        .collect();
    println!("prepared {ROWS} rows in {:?}", prepared_at.elapsed());
    state.feed.replace_all(rows);
    state.feed.set_view_height(height);

    let theme = Theme::light();
    let mut surface = Surface::new(width as u32, height as u32);
    let bounds = Rect::new(0, 0, width, height);
    let mut times = Vec::with_capacity(FRAMES);
    let mut seed = 12345u64;
    for _ in 0..FRAMES {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let dy = ((seed >> 40) % 700) as i32 - 250;
        state.feed.scroll_by(dy);
        let started = Instant::now();
        surface.with_canvas(bounds, |canvas| paint(canvas, &state, &theme));
        times.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!(
        "paint over {FRAMES} frames: p50={:.2} ms p99={:.2} ms max={:.2} ms",
        at(0.5),
        at(0.99),
        times[times.len() - 1]
    );
    assert!(
        at(0.99) <= 4.0,
        "p99 {:.2} ms breaks the 4 ms host budget",
        at(0.99)
    );
}
