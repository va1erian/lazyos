//! Brush size stepping and which tools show the size control.

use super::*;

#[test]
fn stepping_walks_the_size_list_and_clamps_at_the_ends() {
    assert_eq!(step_size(4, 1), 8);
    assert_eq!(step_size(4, -1), 2);
    assert_eq!(step_size(16, 1), 16, "no larger size than the last");
    assert_eq!(step_size(1, -1), 1, "no smaller size than the first");
    assert_eq!(step_size(4, 0), 4);
}

#[test]
fn a_size_between_entries_steps_to_the_next_one_in_that_direction() {
    assert_eq!(step_size(5, 1), 8);
    assert_eq!(step_size(5, -1), 4);
    assert_eq!(step_size(500, 1), 16);
    assert_eq!(step_size(500, -1), 16);
}

#[test]
fn only_tools_that_paint_with_a_width_offer_the_size() {
    for tool in [
        Tool::Brush,
        Tool::Eraser,
        Tool::Line,
        Tool::Rectangle,
        Tool::Ellipse,
    ] {
        assert!(uses_size(tool), "{tool:?}");
    }
    for tool in [Tool::Pencil, Tool::Fill, Tool::Picker] {
        assert!(!uses_size(tool), "{tool:?}");
    }
}
