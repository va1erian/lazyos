//! The feed model's rules: anchoring through prepend, height changes and
//! deletes; clamping; the visible range; and a seeded random walk checked
//! against a naive recomputation.

use std::sync::Arc;

use super::*;
use crate::proto::{FeedItem, Post};
use serde_json::json;

fn item(n: u64) -> Arc<FeedItem> {
    let v = json!({
        "uri": format!("at://did:plc:a/app.bsky.feed.post/{n}"), "cid": "c",
        "author": {"did": "did:plc:a", "handle": "a.test"},
        "record": {"text": format!("post {n}")}
    });
    Arc::new(FeedItem {
        post: Post::from_view(&v).unwrap(),
        reason: None,
    })
}

fn rows(range: std::ops::Range<u64>, height: i32) -> Vec<FeedRow<()>> {
    range
        .map(|n| FeedRow::post(item(n), height, None))
        .collect()
}

fn id(n: u64) -> RowId {
    item(n).row_id()
}

fn feed(count: u64, height: i32, view: i32) -> Feed<()> {
    let mut f = Feed::new();
    f.replace_all(rows(0..count, height));
    f.set_view_height(view);
    f
}

#[test]
fn prefix_sums_and_lookup() {
    let f = feed(10, 100, 250);
    assert_eq!(f.total_height(), 1000);
    assert_eq!(f.row_top(3), 300);
    assert_eq!(f.row_at(0), Some(0));
    assert_eq!(f.row_at(99), Some(0));
    assert_eq!(f.row_at(100), Some(1));
    assert_eq!(f.row_at(5000), Some(9), "past the end is the last row");
    assert_eq!(Feed::<()>::new().row_at(0), None);
}

#[test]
fn the_visible_range_covers_partial_rows() {
    let mut f = feed(10, 100, 250);
    assert_eq!(f.visible(), 0..3);
    f.set_scroll_top(150);
    assert_eq!(f.visible(), 1..4, "rows 1..=3 touch 150..400");
    f.set_scroll_top(100);
    assert_eq!(f.visible(), 1..4);
    assert_eq!(f.anchor(), (id(1), 0));
}

#[test]
fn scrolling_clamps_to_both_ends() {
    let mut f = feed(10, 100, 250);
    f.scroll_by(-50);
    assert_eq!(f.scroll_top(), 0);
    f.scroll_by(5_000);
    assert_eq!(f.scroll_top(), 750);
    assert_eq!(f.visible().end, 10);
    let short = feed(2, 100, 500);
    assert_eq!(short.max_scroll(), 0);
}

#[test]
fn prepending_keeps_the_anchor_row_where_it_is_on_screen() {
    let mut f = feed(10, 100, 250);
    f.set_scroll_top(430); // row 4, 30 px in
    let on_screen = f.screen_top(f.index_of(id(4)).unwrap());
    assert_eq!(on_screen, -30);
    f.prepend(rows(100..105, 120));
    assert_eq!(f.index_of(id(4)), Some(9));
    assert_eq!(f.screen_top(f.index_of(id(4)).unwrap()), on_screen);
}

#[test]
fn a_height_change_above_the_view_does_not_move_the_anchor_row() {
    let mut f = feed(10, 100, 250);
    f.set_scroll_top(430);
    let before = f.screen_top(f.index_of(id(4)).unwrap());
    f.set_heights(&[(id(0), 300), (id(2), 40)]);
    assert_eq!(f.screen_top(f.index_of(id(4)).unwrap()), before);
    assert_eq!(f.total_height(), 1000 + 200 - 60);
}

#[test]
fn a_height_change_of_the_anchor_row_keeps_its_offset_inside_it() {
    let mut f = feed(10, 100, 250);
    f.set_scroll_top(480); // 80 px into row 4
    f.set_heights(&[(id(4), 50)]);
    assert_eq!(f.anchor(), (id(4), 49));
}

#[test]
fn deleting_the_anchor_row_moves_to_the_next() {
    let mut f = feed(5, 100, 150);
    f.set_scroll_top(210);
    assert_eq!(f.anchor(), (id(2), 10));
    f.remove(id(2));
    assert_eq!(f.anchor(), (id(3), 0));
    assert_eq!(f.len(), 4);
    f.remove(id(3));
    f.remove(id(4));
    assert_eq!(
        f.anchor().0,
        id(1),
        "the last row's neighbour is the new anchor"
    );
    f.remove(id(0));
    f.remove(id(1));
    assert!(f.is_empty());
    assert_eq!(f.scroll_top(), 0);
}

#[test]
fn deleting_a_row_above_keeps_the_anchor_on_screen() {
    let mut f = feed(10, 100, 250);
    f.set_scroll_top(430);
    let before = f.screen_top(f.index_of(id(4)).unwrap());
    f.remove(id(1));
    assert_eq!(f.screen_top(f.index_of(id(4)).unwrap()), before);
}

#[test]
fn appending_replaces_the_load_more_row_and_skips_repeats() {
    let mut f = feed(3, 100, 150);
    f.append(vec![FeedRow::load_more("cur".into(), 60)]);
    assert!(matches!(
        f.rows().last().unwrap().kind,
        RowKind::LoadMore { .. }
    ));
    f.append(rows(2..6, 100)); // post 2 is a repeat
    assert_eq!(f.len(), 6, "load more dropped, one repeat skipped");
    assert!(f.rows().iter().all(|r| matches!(r.kind, RowKind::Post(_))));
}

#[test]
fn the_view_on_the_load_more_row_stays_at_the_old_end() {
    let mut f = feed(3, 100, 100);
    f.append(vec![FeedRow::load_more("cur".into(), 100)]);
    f.set_scroll_top(f.max_scroll());
    assert_eq!(f.anchor().0, LOAD_MORE_ID);
    f.append(rows(10..13, 100));
    // The view stays on the end of the old content, the new rows below it.
    assert_eq!(f.scroll_top(), 200);
    assert_eq!(f.visible(), 2..3);
}

#[test]
fn a_shorter_view_after_a_resize_keeps_a_valid_position() {
    let mut f = feed(10, 100, 250);
    f.set_scroll_top(750);
    f.set_view_height(600);
    assert_eq!(f.scroll_top(), 400);
    f.set_view_height(2000);
    assert_eq!(f.scroll_top(), 0);
    assert_eq!(f.visible(), 0..10);
}

#[test]
fn near_covers_the_screens_around_the_view() {
    let mut f = feed(100, 100, 300);
    f.set_scroll_top(5000);
    let near = f.near(2);
    assert_eq!(near, 44..59);
    assert!(near.contains(&f.visible().start) && near.end >= f.visible().end);
}

/// A tiny deterministic generator (the model has no `rand`).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below.max(1)
    }
}

/// Checks every invariant against a recomputation from the rows.
fn check(f: &Feed<()>) {
    let mut top = 0;
    for (i, row) in f.rows().iter().enumerate() {
        assert_eq!(f.row_top(i), top, "row {i}");
        assert_eq!(f.index_of(row.id), Some(i));
        top += row.height;
    }
    assert_eq!(f.total_height(), top);
    let scroll = f.scroll_top();
    assert!(scroll >= 0 && scroll <= f.max_scroll().max(0));
    if f.is_empty() {
        assert_eq!(f.visible(), 0..0);
        return;
    }
    let (anchor_id, offset) = f.anchor();
    let at = f.index_of(anchor_id).expect("the anchor row exists");
    assert!(offset >= 0 && offset < f.rows()[at].height);
    // The visible range is exactly the rows touching [scroll, scroll+view).
    let view = f.view_height();
    let expect: Vec<usize> = (0..f.len())
        .filter(|&i| {
            f.row_top(i) < scroll + view.max(1) && f.row_top(i) + f.rows()[i].height > scroll
        })
        .collect();
    let got: Vec<usize> = f.visible().collect();
    if view > 0 {
        assert_eq!(got, expect);
    }
}

#[test]
fn a_random_walk_keeps_every_invariant() {
    let mut rng = Lcg(0x5eed);
    let mut f: Feed<()> = Feed::new();
    let mut next = 0u64;
    f.set_view_height(300);
    for step in 0..3000 {
        match rng.next(8) {
            0 => {
                let n = rng.next(6) + 1;
                f.append(rows(next..next + n, 40 + rng.next(200) as i32));
                next += n;
            }
            1 => {
                let n = rng.next(4) + 1;
                f.prepend(rows(next..next + n, 40 + rng.next(200) as i32));
                next += n;
            }
            2 if !f.is_empty() => {
                let i = rng.next(f.len() as u64) as usize;
                let rid = f.rows()[i].id;
                f.remove(rid);
            }
            3 if !f.is_empty() => {
                let batch: Vec<(RowId, i32)> = (0..rng.next(5))
                    .map(|_| {
                        let i = rng.next(f.len() as u64) as usize;
                        (f.rows()[i].id, 20 + rng.next(300) as i32)
                    })
                    .collect();
                f.set_heights(&batch);
            }
            4 => f.scroll_by(rng.next(1200) as i32 - 600),
            5 => f.set_view_height(100 + rng.next(900) as i32),
            6 => f.set_scroll_top(rng.next(10_000) as i32),
            7 if step % 50 == 0 => f.replace_all(rows(next..next + 20, 90)),
            _ => {}
        }
        check(&f);
    }
}

#[test]
fn a_prepend_keeps_the_screen_position_over_a_random_walk() {
    let mut rng = Lcg(77);
    let mut f = feed(200, 80, 400);
    let mut next = 1000;
    for _ in 0..200 {
        f.scroll_by(rng.next(500) as i32 - 200);
        let (anchor, _) = f.anchor();
        let at = f.index_of(anchor).unwrap();
        let before = f.screen_top(at);
        let n = rng.next(5) + 1;
        f.prepend(rows(next..next + n, 30 + rng.next(150) as i32));
        next += n;
        let after = f.screen_top(f.index_of(anchor).unwrap());
        // Unless clamped at the very top (it cannot be: prepending only
        // adds height above).
        assert_eq!(after, before);
    }
}
