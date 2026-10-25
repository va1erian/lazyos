use super::*;

fn editing(shape: Shape) -> Model {
    let mut m = Model::builtin();
    m.pick_shape(shape);
    m
}

#[test]
fn snapping_clamps_and_rounds() {
    let m = Model::builtin();
    assert_eq!(m.snapped((13, 3)), (16, 0));
    assert_eq!(m.snapped((-40, 900)), (0, 512));
    let mut free = Model::builtin();
    free.snap = false;
    assert_eq!(free.snapped((13, 3)), (13, 3));
}

#[test]
fn a_builtin_slot_is_customised_once_and_can_be_reset() {
    let mut m = editing(Shape::Busy);
    assert!(m.def().is_none());
    m.customize();
    // Busy starts from the arrow.
    assert_eq!(m.def(), Model::builtin().theme.shapes[0].as_ref());
    let before = m.theme.clone();
    m.customize();
    assert_eq!(m.theme, before);
    m.reset_shape();
    assert!(m.def().is_none());
    assert!(m.undo());
    assert!(m.def().is_some());
}

#[test]
fn dragging_a_vertex_is_one_undo_step() {
    let mut m = editing(Shape::Default);
    let tip = m.hit_vertex((4, 4), 20).unwrap();
    m.checkpoint();
    m.selected = Some(tip);
    m.move_selected((100, 101));
    m.move_selected((200, 203));
    assert_eq!(m.def().unwrap().layers[0].polys[0][0], (200, 200));
    assert!(m.dirty());
    assert!(m.undo());
    assert_eq!(m.def().unwrap().layers[0].polys[0][0], (0, 0));
    assert!(!m.dirty());
    assert!(m.redo());
    assert_eq!(m.def().unwrap().layers[0].polys[0][0], (200, 200));
}

#[test]
fn inserting_a_vertex_splits_the_nearest_edge() {
    let mut m = editing(Shape::Text);
    let count = m.def().unwrap().layers[0].polys[0].len();
    assert!(m.insert_vertex((232, 48), 12));
    let poly = &m.def().unwrap().layers[0].polys[0];
    assert_eq!(poly.len(), count + 1);
    assert_eq!(m.selected.unwrap().vertex, 1);
    assert!(!m.insert_vertex((300, 300), 4), "nothing within reach");
}

#[test]
fn deleting_down_to_nothing_returns_the_builtin_shape() {
    let mut m = editing(Shape::Default);
    let first = Pick {
        layer: 0,
        poly: 0,
        vertex: 0,
    };
    // The arrow is one polygon of six points: three deletions leave a
    // triangle, the fourth removes the polygon, its layer and the drawing.
    for left in [5, 4, 3] {
        m.selected = Some(first);
        assert!(m.delete_selected());
        assert_eq!(m.def().unwrap().layers[0].polys[0].len(), left);
    }
    m.selected = Some(first);
    assert!(m.delete_selected());
    assert!(m.def().is_none());
}

#[test]
fn an_empty_layer_keeps_the_shape_valid_and_undo_keeps_the_layer() {
    let mut m = editing(Shape::Hand);
    assert!(m.add_layer());
    assert!(m.def().unwrap().is_valid());
    assert!(m.theme.draws(Shape::Hand));
    assert!(cursorgfx::format::parse(&cursorgfx::format::serialize(&m.theme)).is_ok());
    m.add_pending((1, 1));
    m.set_colours(0x112233, 0x445566);
    assert!(m.undo());
    assert_eq!(m.layer, 1, "the active layer survives an undo");
    assert_eq!(m.pending.len(), 1, "so does a half-drawn polygon");
}

#[test]
fn a_new_polygon_needs_three_points_and_lands_in_a_light_layer() {
    let mut m = editing(Shape::ResizeH);
    m.reset_shape();
    m.add_pending((10, 10));
    m.add_pending((200, 10));
    assert!(!m.finish_polygon());
    assert_eq!(m.pending.len(), 2);
    m.add_pending((100, 200));
    assert!(m.finish_polygon());
    assert!(m.pending.is_empty());
    let def = m.def().unwrap();
    assert!(def.is_valid());
    assert_eq!(def.layers[0].fill, 0xf0f0f0);
    assert_eq!(def.layers[0].polys[0], [(8, 8), (200, 8), (104, 200)]);
}

#[test]
fn colours_layers_and_hotspot_are_undoable() {
    let mut m = editing(Shape::Hand);
    assert!(m.set_colours(0x1060ff, 0x202020));
    assert_eq!(m.active_layer().unwrap().fill, 0x1060ff);
    assert!(m.toggle_thin());
    assert!(m.add_layer());
    assert_eq!(m.layer, 1);
    assert!(m.def().unwrap().layers[1].polys.is_empty());
    m.set_hotspot((300, 40));
    assert_eq!(m.def().unwrap().hot, (304, 40));
    for _ in 0..4 {
        assert!(m.undo());
    }
    assert!(!m.dirty());
    assert!(!m.undo());
}

#[test]
fn no_edit_without_a_drawing() {
    let mut m = editing(Shape::Busy);
    assert!(!m.set_colours(1, 2));
    assert!(!m.toggle_thin());
    assert!(!m.add_layer());
    assert!(!m.delete_selected());
    assert!(!m.insert_vertex((1, 1), 100));
    m.set_hotspot((9, 9));
    assert!(m.def().is_none());
}

#[test]
fn the_edited_theme_always_serialises_and_parses() {
    let mut m = editing(Shape::Default);
    m.add_pending((50, 50));
    m.add_pending((400, 60));
    m.add_pending((300, 400));
    m.finish_polygon();
    m.set_colours(0xabcdef, 0x010203);
    let text = cursorgfx::format::serialize(&m.theme);
    assert_eq!(cursorgfx::format::parse(&text).unwrap(), m.theme);
}
