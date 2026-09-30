use super::*;

const RED: Pixel = [255, 0, 0, 255];

fn model() -> Model {
    Model::new(32, 32)
}

#[test]
fn a_stroke_is_one_undo_step_and_interpolates() {
    let mut model = model();
    model.set_primary(RED);
    model.begin(0, 0, Side::Primary);
    model.extend(10, 0);
    model.extend(10, 10);
    model.end();
    assert_eq!(model.bitmap().get(5, 0), Some(RED), "no gap in the drag");
    assert_eq!(model.history().undo_len(), 1, "the whole drag is one step");
    model.undo();
    assert_eq!(model.bitmap().get(5, 0), Some(WHITE));
    model.redo();
    assert_eq!(model.bitmap().get(5, 0), Some(RED));
}

#[test]
fn a_shape_preview_is_not_an_undo_step() {
    let mut model = model();
    model.set_tool(Tool::Line);
    model.begin(0, 0, Side::Primary);
    model.extend(10, 10);
    assert!(model.preview().is_some());
    assert_eq!(model.history().undo_len(), 0, "a preview is not history");
    model.end();
    assert!(model.preview().is_none());
    assert_eq!(model.history().undo_len(), 1);
}

#[test]
fn a_cancelled_drag_does_not_leave_the_tool_stuck() {
    let mut model = model();
    model.begin(2, 2, Side::Primary);
    model.cancel();
    assert!(!model.is_dragging());
    model.extend(20, 20);
    assert_eq!(model.history().undo_len(), 1);
}

#[test]
fn redo_is_cleared_by_a_new_action() {
    let mut model = model();
    model.begin(1, 1, Side::Primary);
    model.end();
    model.undo();
    assert!(model.history().can_redo());
    model.begin(5, 5, Side::Primary);
    model.end();
    assert!(!model.history().can_redo());
}

#[test]
fn fill_and_eraser_and_picker_behave() {
    let mut model = model();
    model.set_tool(Tool::Fill);
    model.set_primary(RED);
    model.begin(0, 0, Side::Primary);
    assert_eq!(model.bitmap().get(31, 31), Some(RED));
    assert_eq!(model.history().undo_len(), 1);

    model.set_tool(Tool::Picker);
    model.begin(0, 0, Side::Secondary);
    assert_eq!(model.secondary(), RED);

    model.set_tool(Tool::Eraser);
    model.begin(0, 0, Side::Primary);
    model.end();
    assert_eq!(model.bitmap().get(0, 0), Some(WHITE));
}

#[test]
fn a_zero_size_shape_still_commits_nothing_when_unchanged() {
    let mut model = model();
    model.set_tool(Tool::Rectangle);
    model.begin(4, 4, Side::Primary);
    model.end();
    // A degenerate rectangle is a single dot, which is a real change.
    assert_eq!(model.history().undo_len(), 1);
}

#[test]
fn extreme_coordinates_are_clamped_without_panicking() {
    let mut model = model();
    model.set_tool(Tool::Line);
    model.begin(i32::MIN, i32::MIN, Side::Primary);
    model.extend(i32::MAX, i32::MAX);
    model.end();
    model.set_tool(Tool::Fill);
    model.begin(i32::MAX, i32::MIN, Side::Primary);
    assert!(model.bitmap().contains(0, 0));
}

#[test]
fn load_and_reset_clear_the_history() {
    let mut model = model();
    model.begin(0, 0, Side::Primary);
    model.end();
    model.load(Bitmap::white(8, 8));
    assert_eq!(model.bitmap().size(), (8, 8));
    assert!(!model.history().can_undo());
    model.reset(4, 4);
    assert_eq!(model.bitmap().size(), (4, 4));
    assert!(!model.history().can_undo());
}

#[test]
fn clear_is_an_undoable_step() {
    let mut model = model();
    model.set_primary(RED);
    model.begin(0, 0, Side::Primary);
    model.end();
    model.clear();
    assert_eq!(model.bitmap().get(0, 0), Some(WHITE));
    model.undo();
    assert_eq!(model.bitmap().get(0, 0), Some(RED));
}
