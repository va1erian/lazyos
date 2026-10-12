use xui_core::message::MouseButton;

use super::events::side_of;
use super::state::CanvasState;
use crate::model::{Damage, Document, Side, WHITE};

#[test]
fn side_of_maps_left_and_right_only() {
    assert_eq!(side_of(MouseButton::Left), Some(Side::Primary));
    assert_eq!(side_of(MouseButton::Right), Some(Side::Secondary));
    assert_eq!(side_of(MouseButton::Middle), None);
}

#[test]
fn pointer_positions_map_through_the_viewport() {
    let doc = Document::new(100, 100, WHITE).unwrap();
    let mut state = CanvasState::default();
    state.set_view(xui_core::geometry::Rect::new(0, 0, 40, 40));
    state.update(&doc, Damage::All);
    assert_eq!(state.to_doc(3, 4), (3, 4), "100% maps 1:1");
    state.change_viewport(|vp, doc, view| vp.zoom_to(4.0, (0, 0), doc, view));
    assert_eq!(state.to_doc(3, 4), (0, 1));
    state.change_viewport(|vp, doc, view| {
        vp.pan_by(40, 0, doc, view);
    });
    assert_eq!(state.to_doc(0, 0), (10, 0));
    assert_eq!(state.to_doc(-1, i32::MIN).0, 9, "outside the view still maps");
}

#[test]
fn the_screen_image_is_built_lazily_and_only_when_stale() {
    let doc = Document::new(8, 8, WHITE).unwrap();
    let mut state = CanvasState::default();
    state.set_view(xui_core::geometry::Rect::new(0, 0, 20, 20));
    state.update(&doc, Damage::All);
    let first = state.screen().map(|(i, _)| (i.width(), i.height()));
    assert_eq!(first, Some((8, 8)));
    state.change_viewport(|vp, doc, view| vp.zoom_to(2.0, (0, 0), doc, view));
    assert_eq!(state.screen().map(|(i, _)| i.width()), Some(16));
}
