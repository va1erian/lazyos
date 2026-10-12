//! Adjustments through a selection: only the covered pixels change, a soft
//! edge blends, `adjust_in` ignores the selection, and the history holds one
//! step either way.

use crate::model::ops::{Adjustment, map_rect_masked};
use crate::model::{Bitmap, Model, Rect, Side, Tool, WHITE};

fn drag(model: &mut Model, tool: Tool, from: (i32, i32), to: (i32, i32)) {
    model.set_tool(tool);
    model.begin(from.0, from.1, Side::Primary);
    model.extend(to.0, to.1);
    model.end();
}

fn px(model: &Model, x: i32, y: i32) -> [u8; 4] {
    model.document().layers()[0].pixels.get(x, y).unwrap()
}

#[test]
fn a_rectangular_selection_limits_the_adjustment() {
    let mut model = Model::new(40, 40);
    drag(&mut model, Tool::RectSelect, (10, 10), (19, 19));
    let steps = model.history().undo_len();
    model.adjust(Adjustment::Invert).unwrap();
    assert_eq!(model.history().undo_len(), steps + 1, "one step");
    assert_eq!(px(&model, 15, 15), [0, 0, 0, 255], "inside");
    assert_eq!(px(&model, 5, 5), WHITE, "outside");
    assert_eq!(px(&model, 25, 15), WHITE, "outside, same row");
    model.undo();
    assert_eq!(px(&model, 15, 15), WHITE);
}

#[test]
fn no_selection_adjusts_the_whole_layer() {
    let mut model = Model::new(8, 8);
    model.adjust(Adjustment::Invert).unwrap();
    assert_eq!(px(&model, 0, 0), [0, 0, 0, 255]);
    assert_eq!(px(&model, 7, 7), [0, 0, 0, 255]);
}

#[test]
fn adjust_in_ignores_the_selection() {
    let mut model = Model::new(40, 40);
    drag(&mut model, Tool::RectSelect, (10, 10), (19, 19));
    model
        .adjust_in(Adjustment::Invert, Rect::new(30, 30, 4, 4))
        .unwrap();
    assert_eq!(px(&model, 31, 31), [0, 0, 0, 255], "the rectangle given");
    assert_eq!(px(&model, 15, 15), WHITE, "not the selection");
}

#[test]
fn an_ellipse_selection_follows_its_outline() {
    let mut model = Model::new(60, 60);
    drag(&mut model, Tool::EllipseSelect, (10, 10), (49, 49));
    model.adjust(Adjustment::Invert).unwrap();
    assert_eq!(px(&model, 30, 30), [0, 0, 0, 255], "the middle is inverted");
    assert_eq!(
        px(&model, 11, 11),
        WHITE,
        "the corner of the box is outside"
    );
}

#[test]
fn the_masked_walker_scales_by_coverage_and_skips_transparent_pixels() {
    let mut bitmap = Bitmap::white(4, 1);
    bitmap.put(3, 0, [255, 255, 255, 0]);
    let coverage = |x: i32, _| [0u8, 64, 255, 255][x as usize];
    map_rect_masked(&mut bitmap, Rect::new(0, 0, 4, 1), Some(&coverage), |p| {
        [0, 0, 0, p[3]]
    });
    assert_eq!(bitmap.get(0, 0), Some(WHITE), "coverage 0 keeps the pixel");
    let quarter = bitmap.get(1, 0).unwrap();
    assert!((190..=196).contains(&quarter[0]), "{quarter:?}");
    assert_eq!(bitmap.get(2, 0), Some([0, 0, 0, 255]));
    assert_eq!(
        bitmap.get(3, 0),
        Some([255, 255, 255, 0]),
        "alpha 0 is left"
    );
}

#[test]
fn the_kernel_matches_what_apply_does() {
    // The preview draws with the kernel; OK applies with `apply`. They agree
    // on every pixel of a gradient for every adjustment.
    let mut bitmap = Bitmap::white(16, 16);
    for y in 0..16 {
        for x in 0..16 {
            bitmap.put(x, y, [(x * 16) as u8, (y * 16) as u8, 128, 255]);
        }
    }
    let all = [
        Adjustment::Invert,
        Adjustment::BlackAndWhite,
        Adjustment::Sepia,
        Adjustment::BrightnessContrast {
            brightness: 30,
            contrast: -20,
        },
        Adjustment::HueSaturation {
            hue: 70,
            saturation: 150,
            lightness: 10,
        },
    ];
    for adjustment in all {
        let mut applied = bitmap.clone();
        let whole = applied.rect();
        adjustment.apply(&mut applied, whole);
        let kernel = adjustment.kernel();
        for y in 0..16 {
            for x in 0..16 {
                let want = kernel(bitmap.get(x, y).unwrap());
                assert_eq!(applied.get(x, y), Some(want), "{adjustment:?} at {x},{y}");
            }
        }
    }
}
