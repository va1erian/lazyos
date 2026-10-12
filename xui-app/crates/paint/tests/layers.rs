//! Layers, delta history and the memory budget through the public model API.
//! No window, no `Ui`.

use xui_paint::model::{
    Damage, LayerProps, MAX_SIDE, Model, Rect, Side, Tool, WHITE, required_bytes,
};

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn stroke(model: &mut Model, from: (i32, i32), to: (i32, i32)) {
    model.begin(from.0, from.1, Side::Primary);
    model.extend(to.0, to.1);
    model.end();
}

#[test]
fn tools_draw_on_the_active_layer_only() {
    let mut model = Model::new(16, 16);
    model.set_primary(RED);
    model.add_layer().unwrap();
    assert_eq!(model.document().active(), 1);
    stroke(&mut model, (2, 2), (6, 2));
    assert_eq!(model.document().layer(1).unwrap().pixels.get(4, 2), Some(RED));
    assert_eq!(model.document().layer(0).unwrap().pixels.get(4, 2), Some(WHITE));
    // The flattened picture shows it; the picker reads the active layer.
    assert_eq!(model.flatten().get(4, 2), Some(RED));
    assert!(model.select_layer(0));
    assert_eq!(model.bitmap().get(4, 2), Some(WHITE));
    assert!(!model.select_layer(7));
}

#[test]
fn a_stroke_undoes_and_redoes_exactly_and_reports_its_rectangle() {
    let mut model = Model::new(200, 100);
    model.set_primary(BLUE);
    model.set_tool(Tool::Brush);
    model.set_size(8);
    let original = model.document().clone();
    model.take_damage();
    stroke(&mut model, (10, 10), (150, 60));
    let drawn = model.document().clone();
    let Damage::Rect(rect) = model.take_damage() else {
        panic!("a stroke damages a rectangle");
    };
    assert!(rect.width() < 200 && rect.height() < 100, "{rect:?}");
    assert!(rect.intersect(Rect::new(10, 10, 1, 1)) == Rect::new(10, 10, 1, 1));

    model.undo();
    assert_eq!(model.document(), &original);
    let Damage::Rect(undone) = model.take_damage() else {
        panic!("an undo damages a rectangle");
    };
    let changed = model_rect(&drawn, &original);
    assert_eq!(undone.union(changed), undone, "covers every changed pixel");
    model.redo();
    assert_eq!(model.document(), &drawn);
    assert_eq!(model.history().undo_name(), Some("Brush"));
}

/// The smallest rectangle in which two same-sized documents differ.
fn model_rect(a: &xui_paint::model::Document, b: &xui_paint::model::Document) -> Rect {
    let (pa, pb) = (&a.layer(0).unwrap().pixels, &b.layer(0).unwrap().pixels);
    let mut found = Rect::EMPTY;
    for y in 0..a.height() as i32 {
        for x in 0..a.width() as i32 {
            if pa.get(x, y) != pb.get(x, y) {
                found = found.union(Rect::new(x, y, 1, 1));
            }
        }
    }
    found
}

#[test]
fn hundreds_of_strokes_fit_where_snapshots_held_two() {
    let mut model = Model::new(1024, 1024);
    for i in 0..400 {
        stroke(&mut model, (i, 10), (i + 20, 30));
    }
    // 400 whole-image snapshots would be 1.6 GiB; deltas are well under the cap.
    assert_eq!(model.history().undo_len(), 400);
    assert!(model.history().bytes() < 4 * 1024 * 1024);
}

#[test]
fn fill_is_a_delta_too() {
    let mut model = Model::new(64, 64);
    model.set_primary(RED);
    stroke(&mut model, (0, 32), (63, 32));
    let before = model.document().clone();
    model.set_tool(Tool::Fill);
    model.set_primary(BLUE);
    model.begin(5, 5, Side::Primary);
    let filled = model.document().clone();
    assert_eq!(model.bitmap().get(5, 5), Some(BLUE));
    assert_eq!(model.bitmap().get(5, 40), Some(WHITE), "the other side");
    model.undo();
    assert_eq!(model.document(), &before);
    model.redo();
    assert_eq!(model.document(), &filled);
    assert_eq!(model.history().undo_name(), Some("Fill"));
}

#[test]
fn layer_actions_are_undoable_steps() {
    let mut model = Model::new(8, 8);
    model.add_layer().unwrap();
    model.add_layer().unwrap();
    assert_eq!(model.document().layers().len(), 3);
    let three = model.document().clone();

    model
        .set_layer_props(
            1,
            LayerProps {
                name: "Middle".into(),
                visible: false,
                opacity: 100,
            },
        )
        .unwrap();
    assert!(!model.document().layer(1).unwrap().visible);
    model.move_layer(2, 0).unwrap();
    assert_eq!(model.document().layer(0).unwrap().name, "Layer 3");
    model.delete_layer(0).unwrap();
    assert_eq!(model.document().layers().len(), 2);
    assert_eq!(model.history().undo_name(), Some("Delete Layer"));

    for _ in 0..3 {
        model.undo();
    }
    assert_eq!(model.document(), &three);
    for _ in 0..3 {
        model.redo();
    }
    assert_eq!(model.document().layers().len(), 2);
    assert_eq!(model.document().layer(1).unwrap().name, "Middle");
}

#[test]
fn the_last_layer_stays_and_unchanged_props_are_not_steps() {
    let mut model = Model::new(8, 8);
    assert!(model.delete_layer(0).is_err());
    let props = model.document().layer(0).unwrap().props();
    model.set_layer_props(0, props).unwrap();
    assert!(!model.history().can_undo());
    assert!(model.move_layer(0, 5).is_err());
}

#[test]
fn resizing_undoes_with_every_layer_and_pixel() {
    let mut model = Model::new(32, 32);
    model.set_primary(RED);
    stroke(&mut model, (1, 1), (30, 1));
    model.add_layer().unwrap();
    model.set_primary(BLUE);
    stroke(&mut model, (1, 5), (30, 5));
    let before = model.document().clone();

    model.resize(10, 40).unwrap();
    let after = model.document().clone();
    assert_eq!(after.size(), (10, 40));
    assert_eq!(after.layers().len(), 2);
    assert_eq!(after.layer(0).unwrap().pixels.get(5, 1), Some(RED), "kept");
    assert_eq!(after.layer(0).unwrap().pixels.get(5, 35), Some(WHITE), "bottom pads white");
    assert_eq!(
        after.layer(1).unwrap().pixels.get(5, 35),
        Some([0, 0, 0, 0]),
        "upper layers pad transparent"
    );
    assert_eq!(model.take_damage(), Damage::All);

    model.undo();
    assert_eq!(model.document(), &before);
    model.redo();
    assert_eq!(model.document(), &after);
    assert_eq!(model.history().undo_name(), Some("Resize Image"));
}

#[test]
fn the_budget_refuses_documents_and_says_why() {
    // 16384 x 16384 is 1 GiB for one layer: refused before anything allocates.
    let error = Model::try_new(MAX_SIDE, MAX_SIDE).err().expect("over budget");
    assert!(error.contains("16384 x 16384"), "{error}");
    assert!(error.contains("512 MiB"), "{error}");
    // `new` falls back to the default canvas instead of failing.
    assert_eq!(Model::new(MAX_SIDE, MAX_SIDE).document().size(), (320, 240));

    let mut model = Model::new(64, 64);
    let error = model.resize(MAX_SIDE, MAX_SIDE).unwrap_err();
    assert!(error.contains("not enough memory"), "{error}");
    assert_eq!(model.document().size(), (64, 64), "nothing changed");
    assert!(!model.history().can_undo());
    assert!(model.reset(MAX_SIDE, MAX_SIDE).is_err());
    assert_eq!(model.document().size(), (64, 64));
}

#[test]
fn adding_layers_stops_at_the_budget() {
    // 8192 x 4096 is 128 MiB a layer: with the cache and 64 MiB of undo there
    // is room for exactly two layers. Checked without allocating them.
    assert_eq!(required_bytes(8192, 4096, 2), 3 * 128 * 1024 * 1024);
    let model = Model::new(8, 8);
    assert!(model.check_fits(8192, 4096, 2).is_ok());
    assert!(model.check_fits(8192, 4096, 3).is_err());

    // Add and duplicate go through the same check: squeeze the room out with
    // a large floating bitmap and a small document refuses its next layer.
    let mut model = Model::new(64, 64);
    model.add_layer().unwrap();
    model.set_floating_bytes(449 * 1024 * 1024);
    let error = model.add_layer().unwrap_err();
    assert!(error.contains("not enough memory"), "{error}");
    assert!(model.duplicate_layer().is_err());
    assert_eq!(model.document().layers().len(), 2);
    model.set_floating_bytes(0);
    model.duplicate_layer().unwrap();
    assert_eq!(model.document().layer(2).unwrap().name, "Layer 2 copy");
}

#[test]
fn floating_and_clipboard_bytes_shrink_the_room() {
    let mut model = Model::new(2048, 2048);
    // 16 MiB layer + 16 MiB cache + 64 MiB undo = 96 MiB: 416 MiB free.
    assert!(model.check_extra_fits(400 * 1024 * 1024).is_ok());
    model.set_clipboard_bytes(300 * 1024 * 1024);
    assert!(model.check_extra_fits(400 * 1024 * 1024).is_err());
    assert!(model.check_extra_fits(100 * 1024 * 1024).is_ok());
    model.set_floating_bytes(100 * 1024 * 1024);
    assert!(model.check_extra_fits(100 * 1024 * 1024).is_err());
    // And a resize counts them as well.
    assert!(model.resize(16384, 8192).is_err());
}
