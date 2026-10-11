//! Round-trip tests for the generated `os.lazy.display.cursor.v1` stubs
//! (`docs/cursor-plan.md`): the scope's interface id, the one method and the
//! `CursorShape` wire values `libs/cursorgfx` draws.

use messenger_generated::os_lazy_display_cursor_v1::*;
use messenger_generated::os_lazy_display_v1 as display;

#[test]
fn the_scope_is_its_own_interface() {
    assert_ne!(INTERFACE_ID, display::INTERFACE_ID);
    assert_eq!(METHOD_SETCURSOR, 1);
}

#[test]
fn set_cursor_round_trips() {
    for (surface, shape) in [(1, 0), (u64::MAX, CURSOR_SHAPE_RESIZE_NESW), (42, 3)] {
        let args = SetCursorArgs { surface, shape };
        let body = encode_set_cursor_args(&args).unwrap();
        assert_eq!(decode_set_cursor_args(&body).unwrap(), args);
    }
}

#[test]
fn a_missing_field_decodes_to_the_default_shape() {
    let body = encode_set_cursor_args(&SetCursorArgs {
        surface: 9,
        shape: 0,
    })
    .unwrap();
    let decoded = decode_set_cursor_args(&body).unwrap();
    assert_eq!(decoded.shape, CURSOR_SHAPE_DEFAULT);
    assert_eq!(
        decode_set_cursor_args(&[]).unwrap(),
        SetCursorArgs::default()
    );
}

#[test]
fn truncation_never_panics() {
    let body = encode_set_cursor_args(&SetCursorArgs {
        surface: 7,
        shape: 2,
    })
    .unwrap();
    for cut in 1..body.len() {
        // A prefix either fails to decode or decodes to something else; it
        // never panics.
        let _ = decode_set_cursor_args(&body[..cut]);
    }
}

#[test]
fn the_shape_values_are_the_wire_numbering() {
    assert_eq!(
        [
            CURSOR_SHAPE_DEFAULT,
            CURSOR_SHAPE_BUSY,
            CURSOR_SHAPE_TEXT,
            CURSOR_SHAPE_HAND,
            CURSOR_SHAPE_RESIZE_H,
            CURSOR_SHAPE_RESIZE_V,
            CURSOR_SHAPE_RESIZE_NWSE,
            CURSOR_SHAPE_RESIZE_NESW,
        ],
        [0, 1, 2, 3, 4, 5, 6, 7]
    );
}

#[test]
fn the_method_declares_no_objects() {
    assert!(DECLARED_OBJECTS.is_empty());
}
