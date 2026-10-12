//! Selections on the model: the four tools and their modifiers, every pixel
//! tool clipping to the selection, the wand's tolerance edges, the lasso, the
//! floating selection's lift/drag/commit/cancel cycle with its undo, the
//! clipboard and crop. No window, no `Ui`.

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

#[test]
fn copy_cut_and_paste_round_trip() {
    let mut m = with_block();
    assert!(
        Model::new(4, 4).copy(false).is_err(),
        "copy needs a selection"
    );
    m.copy(false).unwrap();
    let clip = m.clipboard().unwrap();
    assert_eq!(clip.bitmap().size(), (10, 10));
    assert_eq!(clip.offset(), (5, 5));
    assert_eq!(clip.bitmap().get(0, 0), Some(RED));
    assert!(m.reserved().clipboard > 0);
    assert!(
        !m.history().can_undo() || m.history().undo_name() != Some("Copy"),
        "copy is not an edit"
    );
    // Cut erases and is undoable.
    m.cut().unwrap();
    assert_eq!(m.bitmap().get(8, 8), Some(CLEAR));
    assert_eq!(m.history().undo_name(), Some("Cut"));
    m.undo();
    assert_eq!(m.bitmap().get(8, 8), Some(RED));
    // Paste floats at the requested spot, selects it and takes the move tool.
    m.paste((20, 22)).unwrap();
    assert_eq!(m.tool(), Tool::MovePixels);
    assert!(m.is_floating());
    assert_eq!(sel_bounds(&m), Some(Rect::new(20, 22, 10, 10)));
    assert_eq!(m.bitmap().get(25, 27), Some(RED));
    assert_eq!(m.bitmap().get(5, 5), Some(RED), "the source is still there");
    m.commit_float();
    assert_eq!(m.history().undo_name(), Some("Paste"));
    m.undo();
    assert_eq!(m.bitmap().get(25, 27), Some(WHITE));
    assert_eq!(sel_bounds(&m), Some(Rect::new(5, 5, 10, 10)));
}

#[test]
fn a_masked_copy_is_transparent_outside_and_copy_merged_sees_every_layer() {
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::EllipseSelect, (0, 0), (19, 19));
    m.copy(false).unwrap();
    let clip = m.clipboard().unwrap().bitmap().clone();
    assert_eq!(clip.get(0, 0), Some(CLEAR));
    assert_eq!(clip.get(10, 10), Some(WHITE));
    // A top layer with a red dot: Copy takes the active layer only.
    m.add_layer().unwrap();
    m.set_tool(Tool::Pencil);
    m.set_primary(RED);
    m.select_layer(0);
    select(&mut m, Tool::RectSelect, (0, 0), (9, 9));
    m.select_layer(1);
    m.set_tool(Tool::Pencil);
    m.begin(3, 3, Side::Primary);
    m.end();
    m.copy(false).unwrap();
    assert_eq!(m.clipboard().unwrap().bitmap().get(3, 3), Some(RED));
    assert_eq!(m.clipboard().unwrap().bitmap().get(5, 5), Some(CLEAR));
    m.select_layer(0);
    m.copy(true).unwrap();
    assert_eq!(
        m.clipboard().unwrap().bitmap().get(3, 3),
        Some(RED),
        "merged shows the red dot"
    );
    assert_eq!(
        m.clipboard().unwrap().bitmap().get(5, 5),
        Some(WHITE),
        "and the background"
    );
}

#[test]
fn paste_into_a_new_layer_and_a_new_image() {
    let mut m = with_block();
    assert!(m.paste_new_layer((0, 0)).is_err(), "nothing copied yet");
    assert!(m.paste_new_image().is_err());
    m.copy(false).unwrap();
    m.paste_new_layer((20, 20)).unwrap();
    assert_eq!(m.document().layers().len(), 2);
    assert_eq!(m.document().active(), 1);
    m.commit_float();
    assert_eq!(m.document().layer(1).unwrap().pixels.get(25, 25), Some(RED));
    assert_eq!(
        m.document().layer(0).unwrap().pixels.get(25, 25),
        Some(WHITE)
    );
    m.undo();
    m.undo();
    assert_eq!(m.document().layers().len(), 1);
    m.paste_new_image().unwrap();
    assert_eq!(m.document().size(), (10, 10));
    assert_eq!(m.bitmap().get(4, 4), Some(RED));
    assert!(
        m.clipboard().is_some(),
        "the clipboard outlives the document"
    );
    assert!(m.selection().is_none());
}

#[test]
fn a_copy_over_the_budget_is_refused_and_keeps_the_old_clipboard() {
    let mut m = with_block();
    m.copy(false).unwrap();
    let before = m.clipboard().unwrap().clone();
    select(&mut m, Tool::RectSelect, (0, 0), (30, 30));
    m.set_floating_bytes(500 * 1024 * 1024);
    let error = m.copy(false).unwrap_err();
    assert!(error.contains("not enough memory"), "{error}");
    assert_eq!(m.clipboard(), Some(&before));
    assert!(m.cut().is_err());
    assert_eq!(
        m.bitmap().get(0, 0),
        Some(WHITE),
        "a refused cut erases nothing"
    );
    m.set_floating_bytes(0);
    m.clear_clipboard();
    assert!(m.clipboard().is_none() && m.reserved().clipboard == 0);
}

#[test]
fn crop_to_selection_is_one_undoable_document_edit() {
    let mut m = with_block();
    assert!(m.crop_to_selection().is_ok());
    assert_eq!(m.document().size(), (10, 10));
    assert!(m.selection().is_none());
    assert_eq!(m.bitmap().get(0, 0), Some(RED));
    assert_eq!(m.history().undo_name(), Some("Crop to Selection"));
    m.undo();
    assert_eq!(m.document().size(), (40, 40));
    assert_eq!(
        sel_bounds(&m),
        Some(Rect::new(5, 5, 10, 10)),
        "the selection comes back"
    );
    m.redo();
    assert_eq!(m.document().size(), (10, 10));
    assert!(m.crop_to_selection().is_err(), "nothing selected now");
    // An elliptical crop clears its corners on every layer.
    let mut m = Model::new(30, 30);
    select(&mut m, Tool::EllipseSelect, (5, 5), (24, 24));
    m.crop_to_selection().unwrap();
    assert_eq!(m.bitmap().get(0, 0), Some(CLEAR));
    assert_eq!(m.bitmap().get(10, 10), Some(WHITE));
}

#[test]
fn undo_during_a_float_lands_it_first() {
    let mut m = with_block();
    m.set_tool(Tool::MovePixels);
    m.begin_with(8, 8, Side::Primary, NONE);
    m.extend_with(9, 8, NONE);
    m.end();
    m.undo();
    assert!(!m.is_floating());
    assert_eq!(
        m.bitmap().get(5, 5),
        Some(RED),
        "the move itself was undone"
    );
    m.redo();
    assert_eq!(m.bitmap().get(5, 5), Some(CLEAR));
}

/// A tiny xorshift for the seeded fuzz.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> i32 {
        (self.next() % n) as i32
    }
}

/// One random operation on `m`.
fn random_op(m: &mut Model, rng: &mut Rng) {
    let point = |rng: &mut Rng| (rng.below(34) - 5, rng.below(34) - 5);
    let mods = |rng: &mut Rng| Mods {
        ctrl: rng.next().is_multiple_of(4),
        shift: rng.next().is_multiple_of(4),
        alt: rng.next().is_multiple_of(4),
    };
    let side = |rng: &mut Rng| {
        if rng.next().is_multiple_of(5) {
            Side::Secondary
        } else {
            Side::Primary
        }
    };
    match rng.below(18) {
        0..=3 => {
            let tools = [
                Tool::RectSelect,
                Tool::EllipseSelect,
                Tool::Lasso,
                Tool::MagicWand,
                Tool::MoveSelection,
            ];
            let tool = tools[rng.below(5) as usize];
            m.set_tool(tool);
            let (a, b, c) = (point(rng), point(rng), point(rng));
            let (mod_a, side_a) = (mods(rng), side(rng));
            m.begin_with(a.0, a.1, side_a, mod_a);
            m.extend_with(b.0, b.1, mod_a);
            m.extend_with(c.0, c.1, mod_a);
            m.end();
        }
        4..=6 => {
            let tools = [
                Tool::Pencil,
                Tool::Brush,
                Tool::Eraser,
                Tool::Line,
                Tool::Rectangle,
                Tool::Ellipse,
                Tool::Fill,
            ];
            m.set_tool(tools[rng.below(7) as usize]);
            m.set_size(1 + rng.below(8) as u32);
            let (a, b) = (point(rng), point(rng));
            let s = side(rng);
            m.begin(a.0, a.1, s);
            m.extend(b.0, b.1);
            m.end();
        }
        7 => m.select_all(),
        8 => m.deselect(),
        9 => m.invert_selection(),
        10 => m.fill_selection([rng.below(256) as u8, 40, 90, 255]),
        11 => m.erase_selection(),
        12 => {
            m.set_tool(Tool::MovePixels);
            let (a, b) = (point(rng), point(rng));
            m.begin(a.0, a.1, Side::Primary);
            m.extend(b.0, b.1);
            m.end();
        }
        13 => {
            m.set_tool(Tool::MovePixels);
            m.nudge(rng.below(7) - 3, rng.below(7) - 3);
        }
        14 => {
            let _ = m.copy(rng.next().is_multiple_of(2));
            let at = point(rng);
            let _ = if rng.next().is_multiple_of(2) {
                m.paste(at)
            } else {
                m.paste_new_layer(at)
            };
        }
        15 => {
            let _ = m.cut();
        }
        16 => {
            let _ = m.crop_to_selection();
        }
        _ => {
            if rng.next().is_multiple_of(2) {
                m.cancel_float();
            } else {
                m.commit_float();
            }
        }
    }
}

#[test]
fn seeded_fuzz_of_selection_edit_and_undo_sequences() {
    let cases: u64 = std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    for case in 0..cases {
        let seed = 0x9E37_79B9_7F4A_7C15_u64.wrapping_mul(case + 1);
        let mut rng = Rng(seed);
        let mut m = Model::new(24, 24);
        let initial = m.document().clone();
        for step in 0..40 {
            let before = m.document().clone();
            let (floating, undo_len) = (m.is_floating(), m.history().undo_len());
            random_op(&mut m, &mut rng);
            // Whatever happened, the bookkeeping stays coherent.
            assert_eq!(
                m.reserved().floating > 0,
                m.is_floating(),
                "seed {seed:#x} step {step}"
            );
            if let Some(s) = m.selection() {
                assert!(
                    !s.bounds().is_empty(),
                    "seed {seed:#x} step {step}: empty selection"
                );
                assert_eq!(
                    s.bounds().intersect(m.document().bounds()),
                    s.bounds(),
                    "seed {seed:#x} step {step}: selection leaves the picture"
                );
            }
            // A plain step (no float before or after) undoes exactly.
            if !floating && !m.is_floating() && m.history().undo_len() == undo_len + 1 {
                let after = m.document().clone();
                m.undo();
                assert_eq!(m.document(), &before, "seed {seed:#x} step {step}: undo");
                m.redo();
                assert_eq!(m.document(), &after, "seed {seed:#x} step {step}: redo");
            }
        }
        m.commit_float();
        let last = m.document().clone();
        while m.history().can_undo() {
            m.undo();
        }
        assert_eq!(
            m.document(),
            &initial,
            "seed {seed:#x}: undoing everything restores the start"
        );
        while m.history().can_redo() {
            m.redo();
        }
        assert_eq!(
            m.document(),
            &last,
            "seed {seed:#x}: redoing everything returns"
        );
    }
}
