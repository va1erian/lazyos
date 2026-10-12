//! Painting with alpha: values pinned against straight-alpha source-over.

use crate::model::{Model, Side, Tool, WHITE};

const RED_HALF: [u8; 4] = [255, 0, 0, 128];

fn stroke(model: &mut Model, from: (i32, i32), to: (i32, i32)) {
    model.begin(from.0, from.1, Side::Primary);
    model.extend(to.0, to.1);
    model.end();
}

#[test]
fn a_translucent_brush_lays_the_colour_over_the_picture() {
    let mut model = Model::new(16, 16);
    model.set_tool(Tool::Brush);
    model.set_size(1);
    model.set_primary(RED_HALF);
    stroke(&mut model, (3, 3), (3, 3));
    // Half red over opaque white: opaque, pink.
    assert_eq!(model.bitmap().get(3, 3), Some([255, 127, 127, 255]));
    assert_eq!(model.bitmap().get(4, 3), Some(WHITE));
}

#[test]
fn overlapping_dabs_of_one_stroke_do_not_build_up() {
    let mut once = Model::new(16, 16);
    let mut many = Model::new(16, 16);
    for model in [&mut once, &mut many] {
        model.set_tool(Tool::Brush);
        model.set_size(5);
        model.set_primary(RED_HALF);
    }
    stroke(&mut once, (8, 8), (8, 8));
    // The same pixels, visited again and again by a wandering pointer.
    many.begin(8, 8, Side::Primary);
    for to in [(9, 8), (8, 8), (7, 8), (8, 8), (8, 9), (8, 7), (8, 8)] {
        many.extend(to.0, to.1);
    }
    many.end();
    assert_eq!(many.bitmap().get(8, 8), once.bitmap().get(8, 8));
    assert_eq!(once.bitmap().get(8, 8), Some([255, 127, 127, 255]));
}

#[test]
fn brush_over_transparent_keeps_the_colour_and_alpha() {
    let mut model = Model::new(8, 8);
    model.add_layer().unwrap();
    model.set_tool(Tool::Brush);
    model.set_size(1);
    model.set_primary(RED_HALF);
    stroke(&mut model, (2, 2), (2, 2));
    assert_eq!(model.bitmap().get(2, 2), Some(RED_HALF));
    // Undo restores transparency exactly.
    model.undo();
    assert_eq!(model.bitmap().get(2, 2), Some([0, 0, 0, 0]));
}

#[test]
fn the_pencil_overwrites_exactly_and_the_eraser_clears() {
    let mut model = Model::new(8, 8);
    model.set_tool(Tool::Pencil);
    model.set_primary(RED_HALF);
    stroke(&mut model, (1, 1), (1, 1));
    assert_eq!(model.bitmap().get(1, 1), Some(RED_HALF), "no blending");
    model.set_tool(Tool::Eraser);
    model.set_size(1);
    stroke(&mut model, (4, 4), (4, 4));
    assert_eq!(model.bitmap().get(4, 4), Some([0, 0, 0, 0]));
}

#[test]
fn a_shape_with_alpha_blends_and_alpha_zero_paints_nothing() {
    let mut model = Model::new(16, 16);
    model.set_tool(Tool::Line);
    model.set_size(1);
    model.set_primary(RED_HALF);
    stroke(&mut model, (2, 5), (10, 5));
    assert_eq!(model.bitmap().get(6, 5), Some([255, 127, 127, 255]));
    model.set_primary([0, 0, 255, 0]);
    stroke(&mut model, (2, 9), (10, 9));
    assert_eq!(model.bitmap().get(6, 9), Some(WHITE), "invisible ink");
}

#[test]
fn the_picker_reads_alpha_and_the_wand_compares_it() {
    let mut model = Model::new(8, 8);
    model.add_layer().unwrap();
    model.set_tool(Tool::Pencil);
    model.set_primary(RED_HALF);
    stroke(&mut model, (1, 1), (1, 1));
    model.set_tool(Tool::Picker);
    model.begin(1, 1, Side::Secondary);
    model.end();
    assert_eq!(model.secondary(), RED_HALF);
    // The wand at 0% tolerance selects the one half-red pixel, not the
    // transparent sea around it.
    model.set_tool(Tool::MagicWand);
    model.set_wand_tolerance(0);
    model.begin(1, 1, Side::Primary);
    model.end();
    let bounds = model.selection().unwrap().bounds();
    assert_eq!((bounds.width(), bounds.height()), (1, 1));
}
