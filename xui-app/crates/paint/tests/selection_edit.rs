//! Selections on the model, part two: every pixel tool clipping to the
//! selection, fill and erase through the mask, and the floating selection.
//! No window, no `Ui`.

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

fn count(m: &Model, f: impl Fn([u8; 4]) -> bool) -> usize {
    let b = m.bitmap();
    (0..b.height() as i32)
        .flat_map(|y| (0..b.width() as i32).map(move |x| (x, y)))
        .filter(|(x, y)| b.get(*x, *y).is_some_and(&f))
        .count()
}

#[test]
fn every_pixel_tool_stays_inside_the_selection() {
    for mask in ["rect", "ellipse"] {
        for tool in [
            Tool::Pencil,
            Tool::Brush,
            Tool::Eraser,
            Tool::Line,
            Tool::Rectangle,
            Tool::Ellipse,
        ] {
            let mut m = Model::new(40, 40);
            m.set_size(5);
            let original = m.bitmap().clone();
            let kind = if mask == "rect" {
                Tool::RectSelect
            } else {
                Tool::EllipseSelect
            };
            select(&mut m, kind, (10, 10), (29, 29));
            let steps = m.history().undo_len();
            // A drag across the whole picture, and one along its middle.
            drag(&mut m, tool, (0, 0), (39, 39), Side::Primary, NONE);
            drag(&mut m, tool, (12, 12), (37, 37), Side::Primary, NONE);
            let drawn = m.history().undo_len() - steps;
            assert!(drawn >= 1);
            for y in 0..40 {
                for x in 0..40 {
                    if !selected(&m, x, y) {
                        assert_eq!(
                            m.bitmap().get(x, y),
                            original.get(x, y),
                            "{tool:?} through a {mask} touched {x},{y}"
                        );
                    }
                }
            }
            assert!(
                count(&m, |p| p != WHITE) > 0,
                "{tool:?} through a {mask} drew nothing at all"
            );
            for _ in 0..drawn {
                m.undo();
            }
            assert_eq!(m.bitmap(), &original, "{tool:?}: undo restores exactly");
            assert_eq!(m.history().undo_len(), steps);
        }
    }
}

#[test]
fn the_bucket_fills_only_the_selected_part_of_its_region() {
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::RectSelect, (10, 10), (19, 19));
    drag(&mut m, Tool::Fill, (15, 15), (15, 15), Side::Primary, NONE);
    assert_eq!(count(&m, |p| p == [0, 0, 0, 255]), 100);
    assert_eq!(m.bitmap().get(9, 15), Some(WHITE));
    // A click outside the selection does nothing and records nothing.
    let steps = m.history().undo_len();
    m.set_primary(RED);
    drag(&mut m, Tool::Fill, (0, 0), (0, 0), Side::Primary, NONE);
    assert_eq!(count(&m, |p| p == RED), 0);
    assert_eq!(m.history().undo_len(), steps);
    m.undo();
    assert_eq!(count(&m, |p| p != WHITE), 0);
}

#[test]
fn fill_and_erase_selection_use_the_mask() {
    let mut m = Model::new(30, 30);
    m.set_primary(RED);
    m.fill_selection(RED);
    assert_eq!(
        count(&m, |p| p == RED),
        900,
        "no selection means the whole layer"
    );
    m.undo();
    select(&mut m, Tool::EllipseSelect, (5, 5), (24, 24));
    m.fill_selection(RED);
    assert!(m.bitmap().get(15, 15) == Some(RED) && m.bitmap().get(5, 5) == Some(WHITE));
    let filled = count(&m, |p| p == RED);
    assert!(filled > 200 && filled < 400);
    m.erase_selection();
    assert_eq!(m.bitmap().get(15, 15), Some(CLEAR));
    assert_eq!(
        m.bitmap().get(5, 5),
        Some(WHITE),
        "outside the mask is untouched"
    );
    assert_eq!(m.history().undo_name(), Some("Erase Selection"));
    m.undo();
    m.undo();
    assert_eq!(count(&m, |p| p != WHITE), 0);
    // Filling with a half-transparent colour composites over what is there.
    m.fill_selection([0, 0, 255, 128]);
    let p = m.bitmap().get(15, 15).unwrap();
    assert!(p[2] == 255 && p[0] > 100 && p[0] < 140, "{p:?}");
}

/// A model whose layer has a red 10x10 block at (5, 5), the block selected.
fn with_block() -> Model {
    let mut m = Model::new(2, 2);
    let mut bitmap = Bitmap::white(40, 40);
    for y in 5..15 {
        for x in 5..15 {
            bitmap.put(x, y, RED);
        }
    }
    m.load(bitmap).unwrap();
    select(&mut m, Tool::RectSelect, (5, 5), (14, 14));
    m
}

#[test]
fn move_selected_pixels_lifts_drags_and_lands_as_one_step() {
    let mut m = with_block();
    let original = m.bitmap().clone();
    let steps = m.history().undo_len();
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    assert!(m.is_floating());
    assert_eq!(
        m.bitmap().get(8, 8),
        Some(RED),
        "the pixels still cover the hole"
    );
    m.extend_with(18, 12, NONE);
    m.end();
    assert!(
        m.is_floating(),
        "the pixels stay in the air after the mouse is up"
    );
    assert_eq!(m.bitmap().get(15, 9), Some(RED));
    assert_eq!(
        m.bitmap().get(5, 5),
        Some(CLEAR),
        "the hole is where they were"
    );
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(15, 9, 10, 10)),
        "the outline moves with them"
    );
    assert!(
        m.reserved().floating > 0,
        "the float counts against the budget"
    );
    assert_eq!(
        m.history().undo_len(),
        steps,
        "nothing is recorded while floating"
    );
    m.commit_float();
    assert!(!m.is_floating());
    assert_eq!(m.reserved().floating, 0);
    assert_eq!(
        m.history().undo_len(),
        steps + 1,
        "one entry for the lift and the drop"
    );
    assert_eq!(m.history().undo_name(), Some("Move Selected Pixels"));
    let landed = m.bitmap().clone();
    m.undo();
    assert_eq!(m.bitmap(), &original);
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 5, 10, 10)));
    m.redo();
    assert_eq!(m.bitmap(), &landed);
    assert_eq!(sel_bounds(&m), Some(Rect::new(15, 9, 10, 10)));
}

#[test]
fn escape_puts_everything_back_and_records_nothing() {
    let mut m = with_block();
    let original = m.bitmap().clone();
    let steps = m.history().undo_len();
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    m.extend_with(30, 30, NONE);
    m.end();
    m.cancel_float();
    assert!(!m.is_floating());
    assert_eq!(m.bitmap(), &original);
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 5, 10, 10)));
    assert_eq!(m.history().undo_len(), steps);
    assert_eq!(m.reserved().floating, 0);
}

#[test]
fn arrow_nudges_make_one_step_and_changing_tool_lands_the_float() {
    let mut m = with_block();
    let steps = m.history().undo_len();
    m.set_tool(Tool::MovePixels);
    m.nudge(1, 0);
    m.nudge(1, 0);
    m.nudge(0, 10);
    assert!(m.is_floating());
    assert_eq!(m.bitmap().get(7 + 5, 15 + 5), Some(RED));
    m.set_tool(Tool::Pencil);
    assert!(!m.is_floating(), "a tool change lands it");
    assert_eq!(m.history().undo_len(), steps + 1);
    m.undo();
    assert_eq!(m.bitmap().get(5, 5), Some(RED));
}

#[test]
fn a_press_outside_the_pixels_lands_them_and_a_float_may_leave_the_picture() {
    let mut m = with_block();
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    m.extend_with(-2, 30, NONE);
    m.end();
    m.begin_with(39, 0, Side::Primary, NONE);
    assert!(!m.is_floating() && !m.is_dragging());
    assert_eq!(m.history().undo_len(), 2);
    // Half of the block hangs off the left edge: only the visible part lands.
    assert_eq!(m.bitmap().get(0, 27), Some(RED));
    m.undo();
    assert_eq!(m.bitmap().get(8, 8), Some(RED));
}

#[test]
fn the_press_must_start_on_the_selection_to_lift() {
    let mut m = with_block();
    m.set_tool(Tool::MovePixels);
    m.begin_with(30, 30, Side::Primary, NONE);
    assert!(!m.is_floating() && !m.is_dragging());
    // With nothing selected the whole layer moves.
    m.deselect();
    m.begin_with(30, 30, Side::Primary, NONE);
    assert!(m.is_floating());
    m.extend_with(31, 30, NONE);
    m.end();
    m.commit_float();
    assert_eq!(m.bitmap().get(0, 0), Some(CLEAR));
    assert_eq!(m.bitmap().get(6, 5), Some(RED));
}

#[test]
fn a_lift_over_the_budget_is_refused_with_a_notice() {
    let mut m = with_block();
    m.set_clipboard_bytes(500 * 1024 * 1024);
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    assert!(!m.is_floating() && !m.is_dragging());
    let notice = m.take_notice().expect("a reason");
    assert!(notice.contains("not enough memory"), "{notice}");
    assert!(m.take_notice().is_none());
}

#[test]
fn move_selection_moves_the_outline_and_not_the_pixels() {
    let mut m = with_block();
    let original = m.bitmap().clone();
    drag(
        &mut m,
        Tool::MoveSelection,
        (8, 8),
        (13, 10),
        Side::Primary,
        NONE,
    );
    assert_eq!(sel_bounds(&m), Some(Rect::new(10, 7, 10, 10)));
    assert_eq!(m.bitmap(), &original);
    assert_eq!(m.history().undo_name(), Some("Move Selection"));
    m.nudge(-10, 0);
    assert_eq!(sel_bounds(&m), Some(Rect::new(0, 7, 10, 10)));
    m.undo();
    m.undo();
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 5, 10, 10)));
}
