//! Canvas resize on the model: content placement, one undo step, history byte
//! accounting and rejection of bad sizes. No window, no `Ui`.

use xui_paint::model::{Model, Side, Tool, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];

fn red_at_origin(model: &mut Model) {
    model.set_primary(RED);
    model.begin(0, 0, Side::Primary);
    model.end();
}

#[test]
fn growing_keeps_the_content_top_left_and_pads_white() {
    let mut model = Model::new(8, 8);
    red_at_origin(&mut model);
    let before = model.revision();
    model.resize(20, 12).unwrap();
    assert_eq!(model.bitmap().size(), (20, 12));
    assert_eq!(model.bitmap().get(0, 0), Some(RED));
    assert_eq!(model.bitmap().get(19, 11), Some(WHITE));
    assert_eq!(model.bitmap().get(10, 0), Some(WHITE));
    assert_ne!(model.revision(), before, "the view must rebuild its image");
}

#[test]
fn shrinking_crops_and_growing_back_does_not_restore() {
    let mut model = Model::new(16, 16);
    model.set_primary(RED);
    model.begin(12, 12, Side::Primary);
    model.end();
    assert_eq!(model.bitmap().get(12, 12), Some(RED));
    model.resize(8, 8).unwrap();
    assert_eq!(model.bitmap().size(), (8, 8));
    model.resize(16, 16).unwrap();
    assert_eq!(model.bitmap().get(12, 12), Some(WHITE), "cropped is gone");
}

#[test]
fn a_resize_is_one_undo_step_and_undo_restores_the_size() {
    let mut model = Model::new(8, 8);
    red_at_origin(&mut model);
    let steps = model.history().undo_len();
    model.resize(30, 5).unwrap();
    assert_eq!(model.history().undo_len(), steps + 1);
    model.undo();
    assert_eq!(model.bitmap().size(), (8, 8));
    assert_eq!(model.bitmap().get(0, 0), Some(RED));
    model.redo();
    assert_eq!(model.bitmap().size(), (30, 5));
    assert_eq!(model.bitmap().get(0, 0), Some(RED));
}

#[test]
fn history_bytes_follow_snapshots_of_different_sizes() {
    let mut model = Model::new(10, 10);
    model.resize(20, 20).unwrap();
    assert_eq!(model.history().bytes(), 10 * 10 * 4);
    model.resize(5, 5).unwrap();
    assert_eq!(model.history().bytes(), 10 * 10 * 4 + 20 * 20 * 4);
    model.undo();
    // One undo snapshot (10x10) plus one redo snapshot (5x5).
    assert_eq!(model.history().bytes(), 10 * 10 * 4 + 5 * 5 * 4);
    model.undo();
    assert_eq!(model.history().bytes(), 20 * 20 * 4 + 5 * 5 * 4);
    model.redo();
    model.redo();
    assert_eq!(model.bitmap().size(), (5, 5));
    assert_eq!(model.history().bytes(), 10 * 10 * 4 + 20 * 20 * 4);
}

#[test]
fn undo_walks_back_across_edits_and_size_changes() {
    let mut model = Model::new(8, 8);
    red_at_origin(&mut model); // step 1 (8x8)
    model.resize(16, 16).unwrap(); // step 2
    model.set_tool(Tool::Pencil);
    model.begin(12, 12, Side::Primary); // step 3 in the larger canvas
    model.end();
    assert_eq!(model.bitmap().get(12, 12), Some(RED));
    model.undo();
    assert_eq!(model.bitmap().size(), (16, 16));
    assert_eq!(model.bitmap().get(12, 12), Some(WHITE));
    model.undo();
    assert_eq!(model.bitmap().size(), (8, 8));
    model.undo();
    assert_eq!(model.bitmap().get(0, 0), Some(WHITE));
    model.redo();
    model.redo();
    model.redo();
    assert_eq!(model.bitmap().size(), (16, 16));
    assert_eq!(model.bitmap().get(12, 12), Some(RED));
}

#[test]
fn a_new_edit_after_undoing_a_resize_drops_the_redo() {
    let mut model = Model::new(8, 8);
    model.resize(16, 16).unwrap();
    model.undo();
    assert!(model.history().can_redo());
    red_at_origin(&mut model);
    assert!(!model.history().can_redo());
    assert_eq!(model.history().bytes(), 8 * 8 * 4);
}

#[test]
fn invalid_sizes_are_rejected_and_change_nothing() {
    let mut model = Model::new(8, 8);
    for (width, height) in [(0, 8), (8, 0), (1025, 8), (8, 1025), (u32::MAX, 1)] {
        assert!(model.resize(width, height).is_err(), "{width}x{height}");
    }
    assert_eq!(model.bitmap().size(), (8, 8));
    assert_eq!(model.history().undo_len(), 0);
}

#[test]
fn the_limits_are_accepted() {
    let mut model = Model::new(8, 8);
    model.resize(1, 1).unwrap();
    assert_eq!(model.bitmap().size(), (1, 1));
    model.resize(1024, 1024).unwrap();
    assert_eq!(model.bitmap().size(), (1024, 1024));
    assert!(model.history().bytes() >= 4);
}

#[test]
fn resizing_to_the_same_size_is_a_no_op() {
    let mut model = Model::new(8, 8);
    let revision = model.revision();
    model.resize(8, 8).unwrap();
    assert_eq!(model.history().undo_len(), 0);
    assert_eq!(model.revision(), revision);
}

#[test]
fn resizing_mid_stroke_commits_the_stroke_first() {
    let mut model = Model::new(8, 8);
    model.set_primary(RED);
    model.begin(1, 1, Side::Primary);
    model.resize(12, 12).unwrap();
    assert!(!model.is_dragging());
    assert_eq!(model.history().undo_len(), 2, "stroke, then resize");
    model.undo();
    assert_eq!(model.bitmap().size(), (8, 8));
    assert_eq!(model.bitmap().get(1, 1), Some(RED));
}
