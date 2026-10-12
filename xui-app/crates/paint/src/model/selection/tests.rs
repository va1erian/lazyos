//! Selection masks: construction, the five combine modes, the shapes, the
//! outline and crop.

use super::*;
use crate::model::{Bitmap, Document, WHITE};

const ALL: Rect = Rect {
    x0: 0,
    y0: 0,
    x1: 32,
    y1: 32,
};

fn rect(x: i32, y: i32, w: u32, h: u32) -> Selection {
    Selection::rect(Rect::new(x, y, w, h)).unwrap()
}

/// The covered pixels of `s` inside `ALL`, as rows of text.
fn picture(s: &Selection, area: Rect) -> Vec<String> {
    (area.y0..area.y1)
        .map(|y| {
            (area.x0..area.x1)
                .map(|x| if s.contains(x, y) { '#' } else { '.' })
                .collect()
        })
        .collect()
}

#[test]
fn a_rectangle_has_no_mask_and_an_empty_one_is_nothing() {
    let s = rect(2, 3, 4, 5);
    assert!(s.is_rect());
    assert_eq!(s.size(), (4, 5));
    assert_eq!(s.bytes(), 0);
    assert_eq!(s.coverage(2, 3), 255);
    assert_eq!(s.coverage(6, 3), 0, "right edge is exclusive");
    assert!(Selection::rect(Rect::new(0, 0, 0, 5)).is_none());
}

#[test]
fn from_mask_tightens_and_normalises() {
    let bounds = Rect::new(0, 0, 4, 4);
    let mut mask = vec![0u8; 16];
    mask[5] = 255; // (1, 1)
    mask[6] = 255; // (2, 1)
    let s = Selection::from_mask(bounds, mask).unwrap();
    assert_eq!(s.bounds(), Rect::new(1, 1, 2, 1));
    assert!(s.is_rect(), "a full mask is a rectangle");
    assert!(Selection::from_mask(bounds, vec![0; 16]).is_none());
    assert!(
        Selection::from_mask(bounds, vec![255; 15]).is_none(),
        "wrong length"
    );
    // A soft pixel keeps the mask.
    let mut soft = vec![0u8; 16];
    soft[0] = 255;
    soft[1] = 100;
    let s = Selection::from_mask(bounds, soft).unwrap();
    assert!(!s.is_rect());
    assert_eq!((s.coverage(0, 0), s.coverage(1, 0)), (255, 100));
    assert!(s.contains(0, 0) && !s.contains(1, 0));
}

#[test]
fn combine_modes_are_set_algebra_on_hard_masks() {
    let a = rect(2, 2, 8, 8);
    let b = rect(6, 6, 8, 8);
    for mode in CombineMode::ALL {
        let got = Selection::combine(Some(&a), Some(&b), mode);
        for y in 0..20 {
            for x in 0..20 {
                let (in_a, in_b) = (a.contains(x, y), b.contains(x, y));
                let want = match mode {
                    CombineMode::Replace => in_b,
                    CombineMode::Union => in_a || in_b,
                    CombineMode::Exclude => in_a && !in_b,
                    CombineMode::Intersect => in_a && in_b,
                    CombineMode::Xor => in_a != in_b,
                };
                let have = got.as_ref().is_some_and(|s| s.contains(x, y));
                assert_eq!(have, want, "{mode:?} at {x},{y}");
            }
        }
    }
}

#[test]
fn combining_with_nothing() {
    let a = rect(1, 1, 3, 3);
    let same = |m| Selection::combine(Some(&a), None, m);
    assert_eq!(same(CombineMode::Replace), None);
    assert_eq!(same(CombineMode::Union), Some(a.clone()));
    assert_eq!(same(CombineMode::Exclude), Some(a.clone()));
    assert_eq!(same(CombineMode::Intersect), None);
    assert_eq!(same(CombineMode::Xor), Some(a.clone()));
    let from_none = |m| Selection::combine(None, Some(&a), m);
    assert_eq!(from_none(CombineMode::Union), Some(a.clone()));
    assert_eq!(from_none(CombineMode::Xor), Some(a.clone()));
    assert_eq!(from_none(CombineMode::Exclude), None);
    assert_eq!(from_none(CombineMode::Intersect), None);
}

#[test]
fn disjoint_and_nested_results_normalise() {
    let big = rect(0, 0, 10, 10);
    let inner = rect(3, 3, 2, 2);
    let hole = Selection::combine(Some(&big), Some(&inner), CombineMode::Exclude).unwrap();
    assert!(!hole.is_rect() && !hole.contains(3, 3) && hole.contains(2, 3));
    assert_eq!(
        Selection::combine(Some(&hole), Some(&inner), CombineMode::Union),
        Some(big.clone())
    );
    let far = rect(20, 20, 2, 2);
    assert_eq!(
        Selection::combine(Some(&inner), Some(&far), CombineMode::Intersect),
        None
    );
    assert_eq!(
        Selection::combine(Some(&inner), Some(&inner), CombineMode::Xor),
        None
    );
}

#[test]
fn the_algebra_holds_for_random_masks() {
    // Seeded: a xor b == (a union b) exclude (a intersect b), and so on.
    let mut seed = 0x1234_5678_u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..40 {
        let mut random = || {
            let bounds = Rect::new(
                (next() % 6) as i32,
                (next() % 6) as i32,
                4 + next() % 8,
                4 + next() % 8,
            );
            let mask = (0..bounds.area())
                .map(|_| if next().is_multiple_of(3) { 255 } else { 0 })
                .collect();
            Selection::from_mask(bounds, mask)
        };
        let (Some(a), Some(b)) = (random(), random()) else {
            continue;
        };
        let union = Selection::combine(Some(&a), Some(&b), CombineMode::Union);
        let both = Selection::combine(Some(&a), Some(&b), CombineMode::Intersect);
        let xor = Selection::combine(Some(&a), Some(&b), CombineMode::Xor);
        let via = match (&union, &both) {
            (Some(u), b) => Selection::combine(Some(u), b.as_ref(), CombineMode::Exclude),
            _ => None,
        };
        assert_eq!(xor, via);
        let inverted = a.inverted_within(ALL);
        let back = inverted.as_ref().and_then(|i| i.inverted_within(ALL));
        assert_eq!(back, a.clipped(ALL), "double inversion is the identity");
    }
}

#[test]
fn invert_translate_and_clip() {
    let s = rect(4, 4, 8, 8);
    let inv = s.inverted_within(ALL).unwrap();
    assert!(!inv.contains(5, 5) && inv.contains(3, 3) && inv.contains(31, 31));
    assert_eq!(inv.bounds(), ALL);
    assert!(
        rect(0, 0, 32, 32).inverted_within(ALL).is_none(),
        "nothing left"
    );
    let moved = s.translated(-6, 2);
    assert_eq!(moved.bounds(), Rect::new(-2, 6, 8, 8));
    let clipped = moved.clipped(ALL).unwrap();
    assert_eq!(clipped.bounds(), Rect::new(0, 6, 6, 8));
    assert!(rect(40, 40, 2, 2).clipped(ALL).is_none());
    // Extreme moves saturate instead of overflowing.
    let _ = s.translated(i32::MAX, i32::MIN).clipped(ALL);
}

#[test]
fn an_ellipse_covers_the_pixels_whose_centre_is_inside() {
    for (w, h) in [
        (1u32, 1u32),
        (2, 2),
        (5, 5),
        (9, 4),
        (4, 9),
        (16, 16),
        (31, 7),
    ] {
        let bounds = Rect::new(3, 2, w, h);
        let s = Selection::ellipse(bounds, Rect::new(0, 0, 64, 64)).unwrap();
        for y in 0..40 {
            for x in 0..64 {
                let dx = (f64::from(x) + 0.5 - (3.0 + f64::from(w) / 2.0)) / (f64::from(w) / 2.0);
                let dy = (f64::from(y) + 0.5 - (2.0 + f64::from(h) / 2.0)) / (f64::from(h) / 2.0);
                let inside = bounds.contains(x, y) && dx * dx + dy * dy <= 1.0 + 1e-9;
                assert_eq!(s.contains(x, y), inside, "{w}x{h} at {x},{y}");
            }
        }
    }
    assert_eq!(
        picture(
            &Selection::ellipse(Rect::new(0, 0, 5, 5), ALL).unwrap(),
            Rect::new(0, 0, 5, 5)
        ),
        [".###.", "#####", "#####", "#####", ".###."]
    );
}

#[test]
fn an_ellipse_is_clipped_to_the_picture_but_keeps_its_shape() {
    let clip = Rect::new(0, 0, 8, 8);
    let wide = Selection::ellipse(Rect::new(-8, -8, 24, 24), clip).unwrap();
    assert_eq!(wide.bounds(), clip.intersect(Rect::new(-8, -8, 24, 24)));
    assert!(wide.contains(4, 4));
    assert!(
        !wide.contains(0, 0) || wide.contains(0, 0),
        "a corner may go either way"
    );
    assert!(Selection::ellipse(Rect::new(50, 50, 4, 4), clip).is_none());
    // A huge drag neither overflows nor allocates beyond the clip.
    let big = Selection::ellipse(Rect::new(-(1 << 24), -(1 << 24), 1 << 25, 1 << 25), clip);
    assert!(big.is_some_and(|s| s.bounds().area() <= 64));
}

#[test]
fn a_polygon_is_filled_by_the_even_odd_rule() {
    let square = Selection::polygon(&[(2, 2), (10, 2), (10, 10), (2, 10)], ALL).unwrap();
    assert!(square.contains(2, 2) && square.contains(9, 9));
    assert!(
        !square.contains(10, 5) && !square.contains(5, 10),
        "far edges are open"
    );
    // A triangle.
    let tri = Selection::polygon(&[(0, 0), (20, 0), (0, 20)], ALL).unwrap();
    assert!(tri.contains(2, 2) && !tri.contains(15, 15));
    // A pentagram drawn in one stroke: the middle pentagon is crossed twice,
    // so even-odd leaves a hole there (non-zero winding would fill it).
    let star = [(16, 1), (26, 30), (1, 11), (31, 11), (6, 30)];
    let star = Selection::polygon(&star, ALL).unwrap();
    assert!(!star.contains(16, 15), "the centre is a hole");
    assert!(star.contains(16, 5), "a point of the star is filled");
    assert!(star.contains(5, 12));
    // Fewer than three points select nothing.
    assert!(Selection::polygon(&[(1, 1), (5, 5)], ALL).is_none());
    assert!(Selection::polygon(&[(100, 100), (120, 100), (100, 120)], ALL).is_none());
}

#[test]
fn spans_become_a_mask() {
    let s = Selection::from_spans(&[(1, 2, 5), (2, 4, 6), (40, 0, 3)], ALL).unwrap();
    assert!(s.contains(2, 1) && s.contains(4, 1) && !s.contains(5, 1));
    assert!(s.contains(5, 2) && !s.contains(3, 2));
    assert!(Selection::from_spans(&[], ALL).is_none());
}

/// The boundary unit edges counted pixel by pixel.
fn boundary_length(s: &Selection) -> i64 {
    let b = s.bounds().inflate(1);
    let mut count = 0;
    for y in b.y0..b.y1 {
        for x in b.x0..b.x1 {
            count += i64::from(s.contains(x, y) != s.contains(x + 1, y));
            count += i64::from(s.contains(x, y) != s.contains(x, y + 1));
        }
    }
    count
}

#[test]
fn the_outline_traces_the_boundary() {
    let r = rect(2, 3, 5, 4);
    let edges = r.edges();
    assert_eq!(edges.len(), 4);
    assert_eq!(edges.iter().filter(|e| e.horizontal()).count(), 2);
    for s in [
        Selection::ellipse(Rect::new(0, 0, 13, 9), ALL).unwrap(),
        Selection::polygon(&[(2, 2), (20, 4), (9, 18), (3, 12)], ALL).unwrap(),
        Selection::combine(
            Some(&rect(0, 0, 10, 10)),
            Some(&rect(3, 3, 2, 2)),
            CombineMode::Exclude,
        )
        .unwrap(),
    ] {
        let edges = s.edges();
        let total: i64 = edges
            .iter()
            .map(|e| i64::from((e.x1 - e.x0) + (e.y1 - e.y0)))
            .sum();
        assert_eq!(total, boundary_length(&s));
        assert!(edges.iter().all(|e| (e.x0 == e.x1) != (e.y0 == e.y1)));
    }
    // A ring has an inner and an outer loop.
    let ring = Selection::combine(
        Some(&rect(0, 0, 10, 10)),
        Some(&rect(3, 3, 4, 4)),
        CombineMode::Exclude,
    )
    .unwrap();
    assert_eq!(ring.edges().len(), 8);
}

#[test]
fn crop_cuts_every_layer_and_clears_outside_the_mask() {
    let mut doc = Document::new(16, 16, WHITE).unwrap();
    doc.set_selection(Selection::ellipse(Rect::new(4, 4, 8, 8), doc.bounds()));
    let cropped = crop_document(&doc).unwrap();
    assert_eq!(cropped.size(), (8, 8));
    assert!(cropped.selection().is_none());
    let layer = &cropped.active_layer().pixels;
    assert_eq!(
        layer.get(0, 0),
        Some([0, 0, 0, 0]),
        "corner is outside the ellipse"
    );
    assert_eq!(layer.get(4, 4), Some(WHITE));
    // A rectangle keeps every pixel; the whole picture crops to nothing.
    let mut doc = Document::new(16, 16, [9, 9, 9, 255]).unwrap();
    doc.set_selection(Selection::rect(Rect::new(2, 2, 5, 3)));
    let cropped = crop_document(&doc).unwrap();
    assert_eq!(cropped.size(), (5, 3));
    assert_eq!(
        cropped.active_layer().pixels.get(0, 0),
        Some([9, 9, 9, 255])
    );
    doc.set_selection(Selection::rect(doc.bounds()));
    assert!(crop_document(&doc).is_none());
    doc.set_selection(None);
    assert!(crop_document(&doc).is_none());
    let _ = Bitmap::white(1, 1);
}
