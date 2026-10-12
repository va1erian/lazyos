//! Image-wide operations land a floating selection (or a text block) first,
//! so the history stays in order and undo restores the picture.

use crate::model::{
    Adjustment, Anchor, CanvasFill, Flip, Model, Pixel, Rect, Resample, Side, Tool, Turn, WHITE,
};

const RED: Pixel = [255, 0, 0, 255];

/// A 4 x 4 picture with a red pixel at (1, 1).
fn picture() -> Model {
    let mut m = Model::new(4, 4);
    m.set_tool(Tool::Pencil);
    m.set_primary(RED);
    m.begin(1, 1, Side::Primary);
    m.end();
    m
}

/// `picture()` with the whole image selected and dragged: a live float.
fn floating() -> Model {
    let mut m = picture();
    m.set_tool(Tool::RectSelect);
    m.begin(0, 0, Side::Primary);
    m.extend(3, 3);
    m.end();
    m.set_tool(Tool::MovePixels);
    m.begin(1, 1, Side::Primary);
    m.extend(3, 3);
    m.end();
    assert!(m.is_floating());
    m
}

#[test]
fn a_press_while_a_drag_is_live_records_that_drag_first() {
    for tool in [Tool::Pencil, Tool::Line, Tool::Gradient, Tool::Fill] {
        let mut m = Model::new(8, 8);
        m.set_tool(tool);
        m.set_primary(RED);
        m.begin(0, 0, Side::Primary);
        m.extend(6, 0);
        // The release was lost: the next press arrives with the drag live.
        m.begin(0, 6, Side::Primary);
        m.end();
        let steps = m.history().undo_len();
        assert!(steps >= 1, "{tool:?}: the first drag is in the history");
        assert!(!m.is_dragging(), "{tool:?}");
        if tool == Tool::Pencil {
            assert_eq!(steps, 2);
            m.undo();
            assert_eq!(m.bitmap().get(0, 0), Some(RED), "the first stroke stays");
            assert_eq!(m.bitmap().get(0, 6), Some(WHITE));
        }
    }
}

/// Runs `op` on a model with a live float, then lands it and undoes
/// everything: the picture must be the one before the float.
fn assert_restored(name: &str, op: impl FnOnce(&mut Model)) {
    let original = picture();
    let mut m = floating();
    op(&mut m);
    assert!(!m.is_floating(), "{name}: the float landed");
    m.commit_float();
    m.undo_to(original.history().undo_len());
    assert_eq!(
        m.document().layers()[0].pixels,
        original.document().layers()[0].pixels,
        "{name}: undo restores the picture"
    );
}

#[test]
fn image_wide_operations_settle_the_float() {
    assert_restored("flip image", |m| m.flip_image(Flip::Horizontal).unwrap());
    assert_restored("rotate image", |m| m.rotate_image(Turn::Cw90).unwrap());
    assert_restored("resize", |m| {
        m.resize_image(8, 8, Resample::Nearest).unwrap();
    });
    assert_restored("canvas size", |m| {
        m.canvas_size(6, 6, Anchor::Center, CanvasFill::Transparent)
            .unwrap();
    });
    assert_restored("crop", |m| m.crop(Rect::new(0, 0, 2, 2)).unwrap());
    assert_restored("flip layer", |m| m.flip_layer(Flip::Vertical).unwrap());
    assert_restored("rotate layer", |m| m.rotate_layer(Turn::Cw90).unwrap());
    assert_restored("adjust in", |m| {
        m.adjust_in(Adjustment::Invert, Rect::new(0, 0, 4, 4))
            .unwrap();
    });
    assert_restored("flatten", |m| m.flatten_image().unwrap());
    assert_restored("merge down", |m| {
        let _ = m.merge_layer_down();
    });
}
