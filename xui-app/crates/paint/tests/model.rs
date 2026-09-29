//! Model edge cases beyond the in-module unit tests: fast drags, hostile
//! coordinates, the byte cap and failure atomicity. No window, no `Ui`.

use xui_paint::model::{Bitmap, Model, Side, Tool, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn dark(bitmap: &Bitmap, x: i32, y: i32) -> bool {
    matches!(bitmap.get(x, y), Some([r, g, b, _]) if r < 80 && g < 80 && b < 80)
}

#[test]
fn a_fast_drag_leaves_no_gap_across_a_wide_canvas() {
    let mut model = Model::new(320, 120);
    model.begin(0, 0, Side::Primary);
    model.extend(319, 100);
    model.end();
    for x in 0..320 {
        let expected = (x as f32 * 100.0 / 319.0).round() as i32;
        let hit = (expected - 1..=expected + 1).any(|y| dark(model.bitmap(), x, y));
        assert!(hit, "no pixel near ({x}, {expected})");
    }
}

#[test]
fn an_ellipse_is_an_outline_not_a_disc() {
    let mut model = Model::new(64, 64);
    model.set_tool(Tool::Ellipse);
    model.set_size(1);
    model.begin(4, 4, Side::Primary);
    model.extend(60, 60);
    model.end();
    assert!(dark(model.bitmap(), 32, 4), "the top of the ring is drawn");
    assert_eq!(
        model.bitmap().get(32, 32),
        Some(WHITE),
        "the centre is hollow"
    );
}

#[test]
fn changing_colour_does_not_affect_the_canvas_until_a_stroke() {
    let mut model = Model::new(8, 8);
    model.set_primary(RED);
    model.set_secondary(BLUE);
    model.swap_colors();
    assert_eq!(model.primary(), BLUE);
    assert_eq!(model.secondary(), RED);
    assert_eq!(model.bitmap().get(0, 0), Some(WHITE), "no pixels changed");
    assert!(!model.history().can_undo());
}

#[test]
fn the_eraser_paints_white_even_when_the_primary_is_not() {
    let mut model = Model::new(8, 8);
    model.set_primary(RED);
    model.set_tool(Tool::Eraser);
    assert_eq!(model.color_for(Side::Primary), WHITE);
    assert_eq!(model.color_for(Side::Secondary), WHITE);
}

#[test]
fn picking_outside_the_canvas_changes_nothing() {
    let mut model = Model::new(8, 8);
    model.set_tool(Tool::Picker);
    let before = (model.primary(), model.secondary());
    model.begin(-4, 100, Side::Primary);
    assert_eq!((model.primary(), model.secondary()), before);
    assert!(!model.history().can_undo());
}

#[test]
fn a_decode_failure_can_leave_the_document_untouched() {
    let mut model = Model::new(8, 8);
    model.set_primary(RED);
    model.begin(1, 1, Side::Primary);
    model.end();
    // The caller decodes first and only loads on success.
    assert!(Bitmap::decode(b"not a png").is_err());
    assert_eq!(model.bitmap().get(1, 1), Some(RED), "the canvas survived");
    assert!(model.history().can_undo());
}

#[test]
fn every_tool_survives_extreme_coordinates() {
    for tool in Tool::ALL {
        let mut model = Model::new(16, 16);
        model.set_tool(tool);
        model.begin(i32::MIN, i32::MAX, Side::Primary);
        model.extend(i32::MAX, i32::MIN);
        model.end();
        model.cancel();
        assert!(!model.is_dragging(), "{tool:?} left a drag open");
    }
}

#[test]
fn the_history_is_bounded_by_bytes_even_for_large_canvases() {
    let mut model = Model::new(1024, 768);
    for _ in 0..6 {
        model.begin(0, 0, Side::Primary);
        model.extend(500, 400);
        model.end();
    }
    assert!(model.history().bytes() <= model.history().cap());
    assert!(model.history().undo_len() >= 1);
    // Undo still works after eviction.
    model.undo();
    assert!(model.history().can_redo());
}

#[test]
fn a_degenerate_rectangle_is_a_visible_dot() {
    let mut model = Model::new(16, 16);
    model.set_tool(Tool::Rectangle);
    model.begin(8, 8, Side::Primary);
    model.end();
    assert!(dark(model.bitmap(), 8, 8));
    assert_eq!(model.history().undo_len(), 1);
}

#[test]
fn a_fill_on_a_one_pixel_canvas_works() {
    let mut model = Model::new(1, 1);
    model.set_tool(Tool::Fill);
    model.set_primary(RED);
    model.begin(0, 0, Side::Primary);
    assert_eq!(model.bitmap().get(0, 0), Some(RED));
}
