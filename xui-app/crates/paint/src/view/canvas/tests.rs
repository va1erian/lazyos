use super::*;

#[test]
fn side_of_maps_left_and_right_only() {
    assert_eq!(side_of(MouseButton::Left), Some(Side::Primary));
    assert_eq!(side_of(MouseButton::Right), Some(Side::Secondary));
    assert_eq!(side_of(MouseButton::Middle), None);
}

#[test]
fn canvas_at_adds_the_scroll_offset() {
    let state = CanvasState {
        offset: (5, 7),
        ..CanvasState::default()
    };
    assert_eq!(canvas_at(&state, 3, 4), (8, 11));
    assert_eq!(canvas_at(&state, -1, i32::MIN), (4, i32::MIN + 7));
}
