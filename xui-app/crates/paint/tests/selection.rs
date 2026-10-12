//! Selections on the model: the four tools and their modifiers, every pixel
//! tool clipping to the selection, the wand's tolerance edges, the lasso, the
//! floating selection's lift/drag/commit/cancel cycle with its undo, the
//! clipboard and crop. No window, no `Ui`.

#![allow(dead_code, unused_imports)]

use xui_paint::model::{Bitmap, CombineMode, Model, Mods, Rect, Side, Tool, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];
const NONE: Mods = Mods::NONE;
const CTRL: Mods = Mods {
    ctrl: true,
    ..Mods::NONE
};
const ALT: Mods = Mods {
    alt: true,
    ..Mods::NONE
};
const SHIFT: Mods = Mods {
    shift: true,
    ..Mods::NONE
};

/// One drag of `tool` from `a` to `b` with `mods` and `side`.
fn drag(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32), side: Side, mods: Mods) {
    m.set_tool(tool);
    m.begin_with(a.0, a.1, side, mods);
    m.extend_with(b.0, b.1, mods);
    m.end();
}

fn select(m: &mut Model, tool: Tool, a: (i32, i32), b: (i32, i32)) {
    drag(m, tool, a, b, Side::Primary, NONE);
}

fn sel_bounds(m: &Model) -> Option<Rect> {
    m.selection().map(|s| s.bounds())
}

fn selected(m: &Model, x: i32, y: i32) -> bool {
    m.selection().is_some_and(|s| s.contains(x, y))
}

#[test]
fn rectangle_and_ellipse_drags_select_and_undo() {
    let mut m = Model::new(40, 40);
    select(&mut m, Tool::RectSelect, (5, 6), (14, 15));
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(5, 6, 10, 10)),
        "corners are inclusive"
    );
    assert_eq!(m.history().undo_len(), 1);
    assert_eq!(m.history().undo_name(), Some("Rectangle Select"));
    select(&mut m, Tool::EllipseSelect, (0, 0), (19, 19));
    assert!(
        selected(&m, 10, 10) && !selected(&m, 0, 0),
        "an ellipse leaves the corners out"
    );
    assert_eq!(m.history().undo_len(), 2);
    m.undo();
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 6, 10, 10)));
    m.undo();
    assert!(m.selection().is_none());
    m.redo();
    m.redo();
    assert!(selected(&m, 10, 10) && !selected(&m, 0, 0));
}

#[test]
fn a_drag_past_the_picture_is_clipped_to_it() {
    let mut m = Model::new(20, 20);
    select(&mut m, Tool::RectSelect, (10, 10), (500, 500));
    assert_eq!(sel_bounds(&m), Some(Rect::new(10, 10, 10, 10)));
    select(&mut m, Tool::RectSelect, (-50, -50), (3, 3));
    assert_eq!(sel_bounds(&m), Some(Rect::new(0, 0, 4, 4)));
    select(
        &mut m,
        Tool::EllipseSelect,
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX),
    );
    assert!(m.selection().is_some_and(|s| s.bounds().area() <= 400));
}

#[test]
fn shift_makes_a_square_or_a_circle() {
    let mut m = Model::new(60, 60);
    drag(
        &mut m,
        Tool::RectSelect,
        (10, 10),
        (30, 18),
        Side::Primary,
        SHIFT,
    );
    assert_eq!(sel_bounds(&m), Some(Rect::new(10, 10, 21, 21)));
    drag(
        &mut m,
        Tool::RectSelect,
        (40, 40),
        (30, 35),
        Side::Primary,
        SHIFT,
    );
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(30, 30, 11, 11)),
        "grows toward the pointer"
    );
    drag(
        &mut m,
        Tool::EllipseSelect,
        (0, 0),
        (20, 6),
        Side::Primary,
        SHIFT,
    );
    let b = sel_bounds(&m).unwrap();
    assert_eq!(b.width(), b.height());
}

#[test]
fn modifiers_and_buttons_pick_the_combine_mode() {
    let mut m = Model::new(60, 60);
    select(&mut m, Tool::RectSelect, (0, 0), (19, 19));
    // Ctrl adds.
    drag(
        &mut m,
        Tool::RectSelect,
        (30, 30),
        (39, 39),
        Side::Primary,
        CTRL,
    );
    assert!(selected(&m, 5, 5) && selected(&m, 35, 35));
    // The right button removes.
    drag(
        &mut m,
        Tool::RectSelect,
        (0, 0),
        (9, 9),
        Side::Secondary,
        NONE,
    );
    assert!(!selected(&m, 5, 5) && selected(&m, 15, 15) && selected(&m, 35, 35));
    // Alt with the left button keeps only the overlap.
    drag(
        &mut m,
        Tool::RectSelect,
        (10, 10),
        (34, 34),
        Side::Primary,
        ALT,
    );
    assert!(selected(&m, 15, 15), "inside both");
    assert!(!selected(&m, 35, 35), "outside the dragged rectangle");
    // Ctrl with the right button toggles.
    drag(
        &mut m,
        Tool::RectSelect,
        (12, 12),
        (15, 15),
        Side::Secondary,
        CTRL,
    );
    assert!(!selected(&m, 13, 13), "xor of a covered area removes it");
    assert!(selected(&m, 11, 11), "and leaves the rest");
    // The options bar's mode applies without a modifier.
    m.set_sel_mode(CombineMode::Union);
    drag(
        &mut m,
        Tool::RectSelect,
        (50, 50),
        (55, 55),
        Side::Primary,
        NONE,
    );
    assert!(selected(&m, 52, 52) && selected(&m, 17, 17));
}

#[test]
fn a_click_without_a_drag_deselects_in_replace_mode_only() {
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::RectSelect, (2, 2), (12, 12));
    drag(
        &mut m,
        Tool::RectSelect,
        (20, 20),
        (20, 20),
        Side::Primary,
        CTRL,
    );
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(2, 2, 11, 11)),
        "union with nothing keeps it"
    );
    let before = m.history().undo_len();
    select(&mut m, Tool::RectSelect, (20, 20), (20, 20));
    assert!(m.selection().is_none());
    assert_eq!(m.history().undo_len(), before + 1);
    m.undo();
    assert!(m.selection().is_some());
}

#[test]
fn select_all_invert_and_deselect_are_undoable() {
    let mut m = Model::new(16, 16);
    m.invert_selection();
    assert!(
        m.selection().is_none() && !m.history().can_undo(),
        "nothing to invert"
    );
    m.select_all();
    assert_eq!(sel_bounds(&m), Some(Rect::new(0, 0, 16, 16)));
    m.invert_selection();
    assert!(
        m.selection().is_none(),
        "the inverse of everything is nothing"
    );
    m.undo();
    select(&mut m, Tool::RectSelect, (4, 4), (7, 7));
    m.invert_selection();
    assert!(!selected(&m, 5, 5) && selected(&m, 0, 0) && selected(&m, 15, 15));
    assert_eq!(m.history().undo_name(), Some("Invert Selection"));
    m.invert_selection();
    assert_eq!(sel_bounds(&m), Some(Rect::new(4, 4, 4, 4)));
    m.deselect();
    assert!(m.selection().is_none());
    m.undo();
    assert!(m.selection().is_some());
}

#[test]
fn a_long_lasso_exposes_its_points_and_ends_with_every_one_of_them() {
    let mut m = Model::new(200, 200);
    m.set_tool(Tool::Lasso);
    assert!(m.lasso_points().is_empty());
    let ring = |i: i32| {
        let a = f64::from(i) * std::f64::consts::TAU / 400.0;
        (100 + (80.0 * a.cos()) as i32, 100 + (80.0 * a.sin()) as i32)
    };
    let first = ring(0);
    m.begin_with(first.0, first.1, Side::Primary, NONE);
    for i in 1..400 {
        let p = ring(i);
        m.extend_with(p.0, p.1, NONE);
        assert_eq!(m.lasso_points().len(), i as usize + 1, "exact, per move");
    }
    m.end();
    assert!(m.lasso_points().is_empty());
    assert!(selected(&m, 100, 100) && !selected(&m, 5, 5));
    // The last points count: the ring closes, the right edge is in.
    assert!(selected(&m, 170, 100));
    assert_eq!(m.history().undo_len(), 1);
}

#[test]
fn the_lasso_fills_a_polygon_by_the_even_odd_rule() {
    let mut m = Model::new(40, 40);
    m.set_tool(Tool::Lasso);
    m.begin_with(2, 2, Side::Primary, NONE);
    for p in [(30, 2), (30, 30), (2, 30)] {
        m.extend_with(p.0, p.1, NONE);
    }
    m.end();
    assert!(selected(&m, 10, 10) && !selected(&m, 35, 35));
    assert_eq!(m.history().undo_name(), Some("Lasso Select"));
    // A single-stroke pentagram has a hole in the middle.
    m.set_tool(Tool::Lasso);
    m.begin_with(20, 1, Side::Primary, NONE);
    for p in [(30, 38), (1, 14), (38, 14), (10, 38)] {
        m.extend_with(p.0, p.1, NONE);
    }
    m.end();
    assert!(!selected(&m, 20, 20), "the centre is crossed twice");
    assert!(selected(&m, 20, 6));
    // Two points are not an area.
    m.set_tool(Tool::Lasso);
    m.begin_with(3, 3, Side::Primary, NONE);
    m.extend_with(9, 9, NONE);
    m.end();
    assert!(m.selection().is_none());
}

/// A 1-pixel-high strip `[10*i, 0, 0]` for the wand tests.
fn ramp(model: &mut Model) {
    let mut strip = Bitmap::white(16, 1);
    for i in 0..16 {
        strip.put(i, 0, [i as u8 * 10, 0, 0, 255]);
    }
    model.load(strip).unwrap();
}

#[test]
fn the_wand_includes_exactly_the_pixels_within_the_tolerance() {
    let mut m = Model::new(2, 2);
    ramp(&mut m);
    m.set_tool(Tool::MagicWand);
    let run = |m: &mut Model, percent: u8| {
        m.set_wand_tolerance(percent);
        m.begin_with(5, 0, Side::Primary, NONE);
        m.end();
        (0..16).filter(|x| selected(m, *x, 0)).collect::<Vec<i32>>()
    };
    assert_eq!(run(&mut m, 0), [5], "0% is an exact match");
    // 3% is 8/255, 4% is 10/255: the neighbours are 10 away.
    assert_eq!(run(&mut m, 3), [5]);
    assert_eq!(
        run(&mut m, 4),
        [4, 5, 6],
        "a difference equal to the threshold is in"
    );
    assert_eq!(run(&mut m, 8), [3, 4, 5, 6, 7]);
    assert_eq!(run(&mut m, 100).len(), 16, "100% takes everything");
    m.set_wand_tolerance(250);
    assert_eq!(m.wand_tolerance(), 100, "clamped");
}

#[test]
fn the_wand_compares_alpha_too() {
    let mut m = Model::new(2, 2);
    let mut strip = Bitmap::transparent(4, 1);
    strip.put(2, 0, [0, 0, 0, 255]);
    m.load(strip).unwrap();
    m.set_tool(Tool::MagicWand);
    m.set_wand_tolerance(10);
    m.set_wand_contiguous(false);
    m.begin_with(0, 0, Side::Primary, NONE);
    m.end();
    assert!(selected(&m, 0, 0) && selected(&m, 3, 0) && !selected(&m, 2, 0));
}

#[test]
fn the_wand_is_contiguous_or_global() {
    let mut m = Model::new(2, 2);
    let mut strip = Bitmap::white(5, 1);
    strip.put(2, 0, RED);
    m.load(strip).unwrap();
    m.set_tool(Tool::MagicWand);
    m.set_wand_tolerance(0);
    m.begin_with(0, 0, Side::Primary, NONE);
    m.end();
    assert!(
        selected(&m, 1, 0) && !selected(&m, 3, 0),
        "contiguous stops at the wall"
    );
    m.set_wand_contiguous(false);
    m.begin_with(0, 0, Side::Primary, NONE);
    m.end();
    assert!(selected(&m, 3, 0) && selected(&m, 4, 0) && !selected(&m, 2, 0));
    // Ctrl adds a second region; a click off the picture deselects.
    m.set_wand_contiguous(true);
    m.begin_with(2, 0, Side::Primary, CTRL);
    m.end();
    assert!(selected(&m, 2, 0) && selected(&m, 3, 0));
    m.begin_with(-4, 0, Side::Primary, NONE);
    m.end();
    assert!(m.selection().is_none());
}
