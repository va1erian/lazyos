//! Tests of `damage`: the rectangle arithmetic and the rules for what a paint has to do.

use crate::*;

fn r(x: i32, y: i32, w: i32, h: i32) -> IRect {
    IRect::new(x, y, w, h)
}

#[test]
fn subtract_leaves_disjoint_pieces_that_add_up() {
    let outer = r(0, 0, 10, 10);
    for inner in [
        r(2, 3, 4, 5),
        r(-5, -5, 8, 8),
        r(8, 8, 9, 9),
        r(0, 0, 10, 10),
        r(20, 20, 2, 2),
    ] {
        let mut pieces = Vec::new();
        outer.subtract(&inner, &mut pieces);
        let kept: i64 = pieces.iter().map(IRect::area).sum();
        assert_eq!(
            kept,
            outer.area() - outer.intersect(&inner).area(),
            "{inner:?}"
        );
        for (i, a) in pieces.iter().enumerate() {
            assert!(outer.contains(a));
            assert!(a.intersect(&inner).is_empty());
            for b in &pieces[i + 1..] {
                assert!(a.intersect(b).is_empty(), "{a:?} {b:?}");
            }
        }
    }
}

#[test]
fn the_first_paint_is_a_whole_frame() {
    let mut pending = Pending::new(100, 100);
    assert_eq!(pending.take_plan(), None);
    pending.note_rect(r(0, 0, 1, 1));
    assert_eq!(pending.take_plan(), Some(Plan::Full));
    pending.note_rect(r(0, 0, 1, 1));
    assert_eq!(
        pending.take_plan(),
        Some(Plan::Delta {
            scroll: None,
            rects: vec![r(0, 0, 1, 1)]
        })
    );
    assert_eq!(pending.take_plan(), None);
}

#[test]
fn a_scroll_is_a_move_and_the_rows_it_exposed() {
    let mut pending = Pending::new(100, 100);
    pending.take_plan();
    pending.note_full();
    pending.take_plan();
    pending.note_update(
        r(0, 0, 94, 100),
        0,
        -10,
        &[r(0, 90, 94, 10), r(94, 0, 6, 100)],
    );
    match pending.take_plan() {
        Some(Plan::Delta {
            scroll: Some(scroll),
            rects,
        }) => {
            assert_eq!(
                scroll,
                Scroll {
                    area: r(0, 0, 94, 100),
                    dx: 0,
                    dy: -10
                }
            );
            assert_eq!(rects.len(), 2);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn scrolls_of_one_area_add_up_and_carry_their_rectangles() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    let area = r(0, 0, 100, 100);
    pending.note_update(area, 0, -10, &[r(0, 90, 100, 10)]);
    pending.note_update(area, 0, -10, &[r(0, 90, 100, 10)]);
    let Some(Plan::Delta {
        scroll: Some(scroll),
        rects,
    }) = pending.take_plan()
    else {
        panic!("no delta");
    };
    assert_eq!((scroll.dx, scroll.dy), (0, -20));
    // The first strip moved up with the picture: both places are repainted.
    assert!(
        rects.iter().any(|rect| rect.contains(&r(0, 80, 100, 10))),
        "{rects:?}"
    );
    assert!(
        rects.iter().any(|rect| rect.contains(&r(0, 90, 100, 10))),
        "{rects:?}"
    );
}

#[test]
fn a_scroll_and_its_undoing_leave_only_rectangles() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    let area = r(0, 0, 100, 100);
    pending.note_update(area, 0, -10, &[r(0, 90, 100, 10)]);
    pending.note_update(area, 0, 10, &[r(0, 0, 100, 10)]);
    let Some(Plan::Delta { scroll, rects }) = pending.take_plan() else {
        panic!("no delta");
    };
    assert_eq!(scroll, None);
    assert!(!rects.is_empty());
}

#[test]
fn two_different_scrolled_areas_fall_back_to_a_whole_frame() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    pending.note_update(r(0, 0, 100, 100), 0, -5, &[r(0, 95, 100, 5)]);
    pending.note_update(r(0, 0, 50, 100), 0, -5, &[r(0, 95, 50, 5)]);
    assert_eq!(pending.take_plan(), Some(Plan::Full));
}

#[test]
fn a_scroll_further_than_the_area_and_an_update_that_names_nothing_are_whole_frames() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    pending.note_update(r(0, 0, 100, 100), 0, -100, &[r(0, 0, 100, 100)]);
    assert_eq!(pending.take_plan(), Some(Plan::Full));
    pending.note_update(IRect::default(), 0, 0, &[]);
    assert_eq!(pending.take_plan(), Some(Plan::Full));
}

#[test]
fn many_rectangles_and_large_ones_collapse() {
    let mut pending = Pending::new(1000, 1000);
    pending.note_full();
    pending.take_plan();
    for i in 0..20 {
        pending.note_rect(r(i * 40, i * 40, 5, 5));
    }
    let Some(Plan::Delta { rects, .. }) = pending.take_plan() else {
        panic!("no delta");
    };
    assert!(rects.len() <= MAX_RECTS, "{rects:?}");
    for i in 0..20 {
        let wanted = r(i * 40, i * 40, 5, 5);
        assert!(
            rects.iter().any(|rect| rect.contains(&wanted)),
            "{wanted:?} lost in {rects:?}"
        );
    }
    pending.note_rect(r(0, 0, 1000, 900));
    assert_eq!(pending.take_plan(), Some(Plan::Full));
}

#[test]
fn rectangles_are_clipped_and_a_covered_one_is_not_added() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    pending.note_rect(r(90, 90, 50, 50));
    pending.note_rect(r(95, 95, 2, 2));
    let Some(Plan::Delta { rects, .. }) = pending.take_plan() else {
        panic!("no delta");
    };
    assert_eq!(rects, vec![r(90, 90, 10, 10)]);
}

#[test]
fn a_resize_asks_for_a_whole_frame_at_the_new_size() {
    let mut pending = Pending::new(100, 100);
    pending.note_full();
    pending.take_plan();
    pending.resize(50, 60);
    pending.note_rect(r(0, 0, 500, 500));
    assert_eq!(pending.take_plan(), Some(Plan::Full));
    pending.note_rect(r(40, 50, 100, 100));
    assert_eq!(
        pending.take_plan(),
        Some(Plan::Delta {
            scroll: None,
            rects: vec![r(40, 50, 10, 10)]
        })
    );
}
