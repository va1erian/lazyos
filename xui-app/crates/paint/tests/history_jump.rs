//! Jumping through the history, and Layer Properties as one step, through the
//! public model API. No window.

use xui_paint::model::{LayerProps, Model, Side, Tool, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];

fn dot(model: &mut Model, x: i32) {
    model.begin(x, 2, Side::Primary);
    model.end();
}

fn model_with_three_dots() -> Model {
    let mut model = Model::new(16, 16);
    model.set_tool(Tool::Pencil);
    model.set_primary(RED);
    for x in [2, 4, 6] {
        dot(&mut model, x);
    }
    model
}

fn red_dots(model: &Model) -> Vec<i32> {
    (0..16)
        .filter(|x| model.bitmap().get(*x, 2) == Some(RED))
        .collect()
}

#[test]
fn undo_to_and_redo_to_land_on_the_named_step() {
    let mut model = model_with_three_dots();
    assert_eq!(model.history().undo_len(), 3);
    model.undo_to(1);
    assert_eq!(red_dots(&model), vec![2]);
    assert_eq!(model.history().undo_len(), 1);
    assert_eq!(model.history().redo_len(), 2);
    model.redo_to(3);
    assert_eq!(red_dots(&model), vec![2, 4, 6]);
    // Past either end is a clamp, not a panic.
    model.redo_to(99);
    assert_eq!(model.history().undo_len(), 3);
    model.undo_to(0);
    assert_eq!(red_dots(&model), Vec::<i32>::new());
    assert_eq!(model.history().redo_len(), 3);
}

#[test]
fn jump_to_goes_either_way_and_a_new_edit_drops_the_rest() {
    let mut model = model_with_three_dots();
    model.jump_to(0);
    model.jump_to(2);
    assert_eq!(red_dots(&model), vec![2, 4]);
    model.jump_to(1);
    dot(&mut model, 9);
    assert_eq!(red_dots(&model), vec![2, 9]);
    assert!(!model.history().can_redo());
}

#[test]
fn a_jump_damages_what_it_changed() {
    let mut model = model_with_three_dots();
    model.take_damage();
    model.jump_to(0);
    assert_ne!(model.take_damage(), xui_paint::model::Damage::None);
}

#[test]
fn layer_properties_preview_is_not_a_step_and_ok_is_one() {
    let mut model = Model::new(8, 8);
    model.add_layer().unwrap();
    let before = model.history().undo_len();
    let original = model.document().layer(1).unwrap().props();
    let edited = LayerProps {
        name: "Sky".into(),
        visible: true,
        opacity: 128,
    };
    model.preview_layer_props(1, edited.clone());
    assert_eq!(model.document().layer(1).unwrap().opacity, 128);
    assert_eq!(
        model.history().undo_len(),
        before,
        "a preview is not a step"
    );
    model
        .apply_layer_props(1, original.clone(), edited.clone())
        .unwrap();
    assert_eq!(model.history().undo_len(), before + 1);
    assert_eq!(model.history().undo_name(), Some("Layer Properties"));
    model.undo();
    assert_eq!(model.document().layer(1).unwrap().props(), original);
    model.redo();
    assert_eq!(model.document().layer(1).unwrap().props(), edited);
}

#[test]
fn cancelling_a_preview_restores_the_layer_and_the_steps_are_named() {
    let mut model = Model::new(8, 8);
    let original = model.document().layer(0).unwrap().props();
    let mut props = original.clone();
    props.opacity = 10;
    model.preview_layer_props(0, props);
    model.preview_layer_props(0, original.clone());
    assert_eq!(model.document().layer(0).unwrap().props(), original);
    assert!(!model.history().can_undo());

    let mut hidden = original.clone();
    hidden.visible = false;
    model.set_layer_props(0, hidden.clone()).unwrap();
    assert_eq!(model.history().undo_name(), Some("Hide Layer"));
    model.set_layer_props(0, original.clone()).unwrap();
    assert_eq!(model.history().undo_name(), Some("Show Layer"));
    let mut faded = original.clone();
    faded.opacity = 5;
    model.set_layer_props(0, faded).unwrap();
    assert_eq!(model.history().undo_name(), Some("Layer Opacity"));
    let mut renamed = model.document().layer(0).unwrap().props();
    renamed.name = "x".into();
    model.set_layer_props(0, renamed).unwrap();
    assert_eq!(model.history().undo_name(), Some("Rename Layer"));
}

#[test]
fn a_transparent_new_image_starts_clear() {
    let mut model = Model::new(8, 8);
    model.reset_with(10, 6, [0, 0, 0, 0]).unwrap();
    assert_eq!(model.document().size(), (10, 6));
    assert_eq!(model.bitmap().get(3, 3), Some([0, 0, 0, 0]));
    model.reset(4, 4).unwrap();
    assert_eq!(model.bitmap().get(1, 1), Some(WHITE));
}

#[test]
fn layer_opacity_and_visibility_composite_in_the_flattened_picture() {
    let mut model = Model::new(8, 8);
    model.add_layer().unwrap();
    model.set_tool(Tool::Pencil);
    model.set_primary(RED);
    dot(&mut model, 3);
    let props = |opacity, visible| LayerProps {
        name: "Layer 2".into(),
        visible,
        opacity,
    };
    model.set_layer_props(1, props(128, true)).unwrap();
    assert_eq!(model.flatten().get(3, 2), Some([255, 127, 127, 255]));
    model.set_layer_props(1, props(128, false)).unwrap();
    assert_eq!(
        model.flatten().get(3, 2),
        Some(WHITE),
        "hidden adds nothing"
    );
    model.set_layer_props(1, props(0, true)).unwrap();
    assert_eq!(
        model.flatten().get(3, 2),
        Some(WHITE),
        "opacity 0 adds nothing"
    );
    model.set_layer_props(1, props(255, true)).unwrap();
    assert_eq!(model.flatten().get(3, 2), Some(RED), "opaque is exact");
    // Pixel alpha and layer opacity multiply: 128 * 128 / 255 = 64.
    model.set_primary([255, 0, 0, 128]);
    dot(&mut model, 5);
    model.set_layer_props(1, props(128, true)).unwrap();
    assert_eq!(model.flatten().get(5, 2), Some([255, 191, 191, 255]));
}
