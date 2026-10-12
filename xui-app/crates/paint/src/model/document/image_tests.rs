//! The image operations, adjustments and merge/flatten on the `Model`:
//! results, one undo step each, and the budget.

use crate::model::ops::Adjustment;
use crate::model::{
    Anchor, CanvasFill, Flip, LayerProps, Model, Pixel, Rect, Resample, Side, Tool, Turn, WHITE,
};

const RED: Pixel = [255, 0, 0, 255];
const BLUE: Pixel = [0, 0, 255, 255];
const NONE: Pixel = [0, 0, 0, 0];

/// An operation, its history name, the resulting size and where the red and
/// blue pixels end up.
type Case = (
    &'static str,
    Box<dyn Fn(&mut Model)>,
    (u32, u32),
    (i32, i32),
    (i32, i32),
);

/// Draws one pixel of `color` on layer `layer` (leaves that layer active).
fn dot(model: &mut Model, layer: usize, x: i32, y: i32, color: Pixel) {
    model.select_layer(layer);
    model.set_tool(Tool::Pencil);
    model.set_primary(color);
    model.begin(x, y, Side::Primary);
    model.end();
}

fn px(model: &Model, layer: usize, x: i32, y: i32) -> Pixel {
    model.document().layers()[layer].pixels.get(x, y).unwrap()
}

/// The history grew by exactly one step named `name`.
fn one_step(model: &Model, before: usize, name: &str) {
    assert_eq!(model.history().undo_len(), before + 1, "{name}");
    assert_eq!(model.history().undo_name(), Some(name));
}

#[test]
fn resize_image_scales_every_layer_and_undoes() {
    let mut m = Model::new(4, 2);
    dot(&mut m, 0, 0, 0, RED);
    m.add_layer().unwrap();
    dot(&mut m, 1, 3, 1, BLUE);
    let steps = m.history().undo_len();
    m.resize_image(8, 4, Resample::Nearest).unwrap();
    one_step(&m, steps, "Resize Image");
    assert_eq!(m.document().size(), (8, 4));
    assert!(
        m.document()
            .layers()
            .iter()
            .all(|l| l.pixels.size() == (8, 4))
    );
    assert_eq!(px(&m, 0, 1, 1), RED, "a pixel became a 2 x 2 block");
    assert_eq!(px(&m, 0, 2, 0), WHITE);
    assert_eq!(px(&m, 1, 7, 3), BLUE);
    assert_eq!(px(&m, 1, 0, 0), NONE);
    m.undo();
    assert_eq!(m.document().size(), (4, 2));
    assert_eq!(px(&m, 0, 0, 0), RED);
    assert_eq!(px(&m, 1, 3, 1), BLUE);
    m.redo();
    assert_eq!(m.document().size(), (8, 4));
    // Bilinear shrinks too, and the same size is no step.
    let steps = m.history().undo_len();
    m.resize_image(4, 2, Resample::Bilinear).unwrap();
    m.resize_image(4, 2, Resample::Bilinear).unwrap();
    assert_eq!(m.history().undo_len(), steps + 1);
}

#[test]
fn resize_image_checks_the_sides_and_the_budget() {
    let mut m = Model::new(8192, 4096);
    m.add_layer().unwrap();
    let before = m.document().clone();
    for (w, h) in [(0, 5), (5, 0), (16385, 5), (u32::MAX, 5)] {
        assert!(
            m.resize_image(w, h, Resample::Nearest).is_err(),
            "{w} x {h}"
        );
    }
    let err = m.resize_image(16384, 4096, Resample::Nearest).unwrap_err();
    assert!(err.contains("memory"), "{err}");
    assert!(
        m.canvas_size(16384, 8192, Anchor::Center, CanvasFill::Transparent)
            .is_err()
    );
    assert!(m.rotate_image(Turn::Cw90).is_ok(), "same area still fits");
    m.undo();
    assert_eq!(m.document(), &before);
    assert_eq!(m.history().undo_len(), 0);
}

#[test]
fn canvas_size_anchors_and_fills() {
    let mut m = Model::new(4, 4);
    m.set_secondary([9, 9, 9, 255]);
    dot(&mut m, 0, 0, 0, RED);
    m.add_layer().unwrap();
    dot(&mut m, 1, 3, 3, BLUE);
    let steps = m.history().undo_len();
    m.canvas_size(6, 6, Anchor::BottomRight, CanvasFill::Secondary)
        .unwrap();
    one_step(&m, steps, "Canvas Size");
    assert_eq!(px(&m, 0, 2, 2), RED, "shifted by (2, 2)");
    assert_eq!(
        px(&m, 0, 0, 0),
        [9, 9, 9, 255],
        "the bottom layer gets the secondary colour"
    );
    assert_eq!(px(&m, 1, 0, 0), NONE, "layers above stay transparent");
    assert_eq!(px(&m, 1, 5, 5), BLUE);
    m.undo();
    assert_eq!(m.document().size(), (4, 4));
    assert_eq!(px(&m, 0, 0, 0), RED);
    // Transparent fill, a corner anchor, then a centred shrink crops.
    m.canvas_size(6, 5, Anchor::TopLeft, CanvasFill::Transparent)
        .unwrap();
    assert_eq!(px(&m, 0, 0, 0), RED);
    assert_eq!(px(&m, 0, 5, 4), NONE);
    m.canvas_size(2, 2, Anchor::Center, CanvasFill::Transparent)
        .unwrap();
    assert_eq!(m.document().size(), (2, 2));
    assert!(
        m.canvas_size(0, 2, Anchor::Center, CanvasFill::Transparent)
            .is_err()
    );
    assert!(
        m.canvas_size(2, 2, Anchor::Center, CanvasFill::Transparent)
            .is_ok()
    );
}

#[test]
fn crop_keeps_a_rectangle_of_every_layer() {
    let mut m = Model::new(10, 10);
    dot(&mut m, 0, 3, 4, RED);
    m.add_layer().unwrap();
    dot(&mut m, 1, 4, 5, BLUE);
    let steps = m.history().undo_len();
    m.crop(Rect::new(3, 4, 2, 2)).unwrap();
    one_step(&m, steps, "Crop to Selection");
    assert_eq!(m.document().size(), (2, 2));
    assert_eq!(px(&m, 0, 0, 0), RED);
    assert_eq!(px(&m, 1, 1, 1), BLUE);
    m.undo();
    assert_eq!(m.document().size(), (10, 10));
    assert_eq!(px(&m, 0, 3, 4), RED);
    // Clipped to the image; outside or empty is an error; the whole is a no-op.
    m.crop(Rect::new(-5, -5, 8, 8)).unwrap();
    assert_eq!(m.document().size(), (3, 3));
    assert!(m.crop(Rect::new(30, 30, 2, 2)).is_err());
    assert!(m.crop(Rect::EMPTY).is_err());
    let steps = m.history().undo_len();
    m.crop(Rect::new(-9, -9, 99, 99)).unwrap();
    assert_eq!(m.history().undo_len(), steps);
    m.crop(Rect::from_points(
        (i32::MIN, i32::MIN),
        (i32::MAX, i32::MAX),
    ))
    .unwrap();
    assert_eq!(m.document().size(), (3, 3));
}

#[test]
fn flip_and_rotate_image_move_every_layer() {
    let mut m = Model::new(3, 2);
    dot(&mut m, 0, 0, 0, RED);
    m.add_layer().unwrap();
    dot(&mut m, 1, 2, 1, BLUE);
    let original = m.document().clone();
    let cases: [Case; 5] = [
        (
            "Flip Image Horizontal",
            Box::new(|m| m.flip_image(Flip::Horizontal).unwrap()),
            (3, 2),
            (2, 0),
            (0, 1),
        ),
        (
            "Flip Image Vertical",
            Box::new(|m| m.flip_image(Flip::Vertical).unwrap()),
            (3, 2),
            (0, 1),
            (2, 0),
        ),
        (
            "Rotate Image 90 CW",
            Box::new(|m| m.rotate_image(Turn::Cw90).unwrap()),
            (2, 3),
            (1, 0),
            (0, 2),
        ),
        (
            "Rotate Image 90 CCW",
            Box::new(|m| m.rotate_image(Turn::Ccw90).unwrap()),
            (2, 3),
            (0, 2),
            (1, 0),
        ),
        (
            "Rotate Image 180",
            Box::new(|m| m.rotate_image(Turn::Half).unwrap()),
            (3, 2),
            (2, 1),
            (0, 0),
        ),
    ];
    for (name, apply, size, red, blue) in cases {
        apply(&mut m);
        one_step(&m, 3, name);
        assert_eq!(m.document().size(), size, "{name}");
        assert_eq!(px(&m, 0, red.0, red.1), RED, "{name}");
        assert_eq!(px(&m, 1, blue.0, blue.1), BLUE, "{name}");
        assert_eq!(m.document().layers().len(), 2);
        m.undo();
        assert_eq!(m.document(), &original, "{name} undone");
        m.redo();
        m.undo();
    }
}

#[test]
fn flip_and_rotate_layer_leave_the_others_alone() {
    let mut m = Model::new(3, 3);
    dot(&mut m, 0, 0, 0, RED);
    m.add_layer().unwrap();
    dot(&mut m, 1, 0, 0, BLUE);
    let steps = m.history().undo_len();
    m.flip_layer(Flip::Horizontal).unwrap();
    one_step(&m, steps, "Flip Layer Horizontal");
    assert_eq!(px(&m, 1, 2, 0), BLUE);
    assert_eq!(px(&m, 1, 0, 0), NONE);
    assert_eq!(px(&m, 0, 0, 0), RED, "the other layer did not move");
    m.rotate_layer(Turn::Cw90).unwrap();
    assert_eq!(m.history().undo_name(), Some("Rotate Layer 90 CW"));
    assert_eq!(px(&m, 1, 2, 2), BLUE, "top-right turned to bottom-right");
    m.undo();
    m.undo();
    assert_eq!(px(&m, 1, 0, 0), BLUE);
    assert_eq!(m.document().active(), 1);
    // A layer that would not change records nothing.
    m.select_layer(1);
    m.clear();
    m.undo();
    let steps = m.history().undo_len();
    m.add_layer().unwrap();
    m.flip_layer(Flip::Vertical).unwrap();
    m.rotate_layer(Turn::Half).unwrap();
    assert_eq!(m.history().undo_len(), steps + 1, "only the add");
}

#[test]
fn rotating_a_layer_in_a_wide_image_keeps_the_image_size() {
    let mut m = Model::new(4, 2);
    dot(&mut m, 0, 1, 1, RED);
    m.rotate_layer(Turn::Cw90).unwrap();
    assert_eq!(m.document().size(), (4, 2));
    assert_eq!(m.document().layers()[0].pixels.size(), (4, 2));
    // (1,1) of 4x2 -> (0,1) of 2x4 -> placed at (1,-1): (1,0).
    assert_eq!(px(&m, 0, 1, 0), RED);
    m.undo();
    assert_eq!(px(&m, 0, 1, 1), RED);
}

#[test]
fn flatten_composites_the_visible_layers_into_one() {
    let mut m = Model::new(4, 4);
    m.add_layer().unwrap();
    dot(&mut m, 1, 1, 1, RED);
    m.set_layer_props(
        1,
        LayerProps {
            name: "Red".into(),
            visible: true,
            opacity: 128,
        },
    )
    .unwrap();
    m.add_layer().unwrap();
    dot(&mut m, 2, 2, 2, [0, 255, 0, 255]);
    m.set_layer_props(
        2,
        LayerProps {
            name: "Hidden".into(),
            visible: false,
            opacity: 255,
        },
    )
    .unwrap();
    let expected = m.flatten();
    let layers = m.document().clone();
    let steps = m.history().undo_len();
    m.flatten_image().unwrap();
    one_step(&m, steps, "Flatten");
    assert_eq!(m.document().layers().len(), 1);
    let only = &m.document().layers()[0];
    assert_eq!(
        (only.name.as_str(), only.opacity, only.visible),
        ("Background", 255, true)
    );
    assert_eq!(only.pixels, expected);
    assert_eq!(px(&m, 0, 1, 1), [255, 127, 127, 255]);
    assert_eq!(px(&m, 0, 2, 2), WHITE, "the hidden layer is dropped");
    m.undo();
    assert_eq!(
        m.document(),
        &layers,
        "names, opacity and visibility come back"
    );
    m.redo();
    assert_eq!(m.document().layers().len(), 1);
    // A single layer is left alone.
    let steps = m.history().undo_len();
    m.flatten_image().unwrap();
    assert_eq!(m.history().undo_len(), steps);
}

#[test]
fn merge_layer_down_composites_through_the_opacity() {
    let mut m = Model::new(4, 4);
    m.add_layer().unwrap();
    dot(&mut m, 1, 1, 1, RED);
    m.set_layer_props(
        1,
        LayerProps {
            name: "Red".into(),
            visible: true,
            opacity: 128,
        },
    )
    .unwrap();
    let expected = m.flatten();
    let two_layers = m.document().clone();
    let steps = m.history().undo_len();
    m.merge_layer_down().unwrap();
    one_step(&m, steps, "Merge Layer Down");
    assert_eq!(m.document().layers().len(), 1);
    assert_eq!(m.document().active(), 0);
    assert_eq!(px(&m, 0, 1, 1), [255, 127, 127, 255]);
    assert_eq!(px(&m, 0, 0, 0), WHITE);
    assert_eq!(
        m.document().layers()[0].name,
        "Background",
        "the lower layer's name stays"
    );
    assert_eq!(m.flatten(), expected, "the picture does not change");
    m.undo();
    assert_eq!(m.document(), &two_layers);
    assert_eq!(m.document().active(), 1);
    m.redo();
    assert_eq!(px(&m, 0, 1, 1), [255, 127, 127, 255]);
    m.undo();
    m.redo();
    m.undo();
    assert_eq!(m.document(), &two_layers, "any number of undo/redo cycles");
}

#[test]
fn merge_layer_down_in_the_middle_of_a_stack() {
    let mut m = Model::new(4, 4);
    m.add_layer().unwrap();
    m.add_layer().unwrap();
    dot(&mut m, 0, 0, 0, [1, 1, 1, 255]);
    dot(&mut m, 1, 1, 0, [2, 2, 2, 255]);
    dot(&mut m, 2, 2, 0, [3, 3, 3, 255]);
    let names: Vec<_> = m
        .document()
        .layers()
        .iter()
        .map(|l| l.name.clone())
        .collect();
    m.select_layer(1);
    let before = m.document().clone();
    m.merge_layer_down().unwrap();
    assert_eq!(m.document().layers().len(), 2);
    assert_eq!(
        m.document().layers()[1].name,
        names[2],
        "the layer above is kept"
    );
    assert_eq!(px(&m, 0, 0, 0), [1, 1, 1, 255]);
    assert_eq!(px(&m, 0, 1, 0), [2, 2, 2, 255]);
    assert_eq!(px(&m, 1, 2, 0), [3, 3, 3, 255]);
    m.undo();
    assert_eq!(m.document(), &before);
    // A hidden layer adds nothing but is still removed.
    m.set_layer_props(
        1,
        LayerProps {
            name: "H".into(),
            visible: false,
            opacity: 255,
        },
    )
    .unwrap();
    m.merge_layer_down().unwrap();
    assert_eq!(px(&m, 0, 1, 0), WHITE);
    assert_eq!(m.document().layers().len(), 2);
    // The bottom layer has nothing below it.
    m.select_layer(0);
    assert!(m.merge_layer_down().is_err());
    assert_eq!(m.document().layers().len(), 2);
}

#[test]
fn adjustments_are_undoable_edits_of_the_active_layer() {
    let mut m = Model::new(4, 4);
    dot(&mut m, 0, 1, 1, [10, 20, 30, 255]);
    let original = m.document().clone();
    let steps = m.history().undo_len();
    m.adjust(Adjustment::Invert).unwrap();
    one_step(&m, steps, "Invert Colors");
    assert_eq!(px(&m, 0, 0, 0), [0, 0, 0, 255]);
    assert_eq!(px(&m, 0, 1, 1), [245, 235, 225, 255]);
    m.undo();
    assert_eq!(m.document(), &original);
    m.redo();
    assert_eq!(px(&m, 0, 1, 1), [245, 235, 225, 255]);
    m.undo();
    // A rectangle limits it (what a selection will pass).
    m.adjust_in(Adjustment::Invert, Rect::new(1, 1, 1, 1))
        .unwrap();
    assert_eq!(px(&m, 0, 1, 1), [245, 235, 225, 255]);
    assert_eq!(px(&m, 0, 0, 0), WHITE);
    m.undo();
    // Names for the History panel.
    for (adjustment, name) in [
        (Adjustment::BlackAndWhite, "Black and White"),
        (Adjustment::Sepia, "Sepia"),
        (
            Adjustment::BrightnessContrast {
                brightness: 10,
                contrast: 0,
            },
            "Brightness/Contrast",
        ),
        (
            Adjustment::HueSaturation {
                hue: 90,
                saturation: 100,
                lightness: 0,
            },
            "Hue/Saturation",
        ),
    ] {
        let steps = m.history().undo_len();
        m.adjust(adjustment).unwrap();
        one_step(&m, steps, name);
        m.undo();
        assert_eq!(m.document(), &original, "{name}");
    }
}

#[test]
fn an_adjustment_that_changes_nothing_records_nothing() {
    let mut m = Model::new(4, 4);
    let steps = m.history().undo_len();
    m.adjust(Adjustment::BrightnessContrast {
        brightness: 0,
        contrast: 0,
    })
    .unwrap();
    m.adjust(Adjustment::HueSaturation {
        hue: 0,
        saturation: 100,
        lightness: 0,
    })
    .unwrap();
    m.adjust(Adjustment::BlackAndWhite).unwrap();
    assert_eq!(m.history().undo_len(), steps, "white is already grey");
    // A transparent layer has no visible colour to change.
    m.add_layer().unwrap();
    let steps = m.history().undo_len();
    m.adjust(Adjustment::Invert).unwrap();
    assert_eq!(m.history().undo_len(), steps);
    assert_eq!(px(&m, 1, 0, 0), NONE);
    // A rectangle outside the layer is nothing to do.
    m.adjust_in(Adjustment::Invert, Rect::new(50, 50, 4, 4))
        .unwrap();
    assert_eq!(m.history().undo_len(), steps);
    // Only the active layer changes.
    m.select_layer(0);
    m.adjust(Adjustment::Invert).unwrap();
    assert_eq!(px(&m, 0, 0, 0), [0, 0, 0, 255]);
    assert_eq!(px(&m, 1, 0, 0), NONE);
}
