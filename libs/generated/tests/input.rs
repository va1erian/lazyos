//! Round-trip tests for the generated `os.lazy.input.v1` and
//! `os.lazy.input.shell.v1` stubs (`docs/input-plan.md`).

use messenger_generated::os_lazy_input_shell_v1 as shell;
use messenger_generated::os_lazy_input_v1 as input;

#[test]
fn interface_ids_differ_and_method_ids_are_pinned() {
    assert_ne!(input::INTERFACE_ID, shell::INTERFACE_ID);
    // Append-only wire numbers: clients and `inputd` are built separately.
    assert_eq!(
        [
            input::METHOD_OPEN,
            input::METHOD_CLOSE,
            input::METHOD_GETSTATE,
            input::METHOD_KEYEVENT,
            input::METHOD_TEXTINPUT,
            input::METHOD_KEYBOARDENTER,
            input::METHOD_KEYBOARDLEAVE,
            input::METHOD_LAYOUTCHANGED,
        ],
        [1, 2, 3, 10, 11, 12, 13, 14]
    );
    assert_eq!(
        [
            shell::METHOD_ATTACH,
            shell::METHOD_SETFOCUS,
            shell::METHOD_REGISTERSURFACE,
            shell::METHOD_UNREGISTERSURFACE,
            shell::METHOD_REGISTERHOTKEY,
            shell::METHOD_UNREGISTERHOTKEY,
            shell::METHOD_APPROVEGRANT,
            shell::METHOD_SETBOUNDS,
            shell::METHOD_GETPOINTER,
            shell::METHOD_HOTKEYFIRED,
            shell::METHOD_GRANTREQUESTED,
            shell::METHOD_ESCAPECHORD,
            shell::METHOD_SESSIONOPENED,
            shell::METHOD_SESSIONCLOSED,
            shell::METHOD_POINTEREVENT,
        ],
        [1, 2, 3, 4, 5, 6, 7, 8, 9, 20, 21, 22, 23, 24, 25]
    );
    assert_eq!(
        [
            input::KEY_STATE_DOWN,
            input::KEY_STATE_UP,
            input::KEY_STATE_REPEAT
        ],
        [0, 1, 2]
    );
}

#[test]
fn key_event_roundtrips_every_field() {
    for state in [
        input::KEY_STATE_DOWN,
        input::KEY_STATE_UP,
        input::KEY_STATE_REPEAT,
    ] {
        let event = input::KeyEventArgs {
            code: 0xE6,
            sym: 0xFFEA,
            mods: 0xFF,
            state,
            ts_ns: u64::MAX,
            seq: 1 << 40,
        };
        let body = input::encode_key_event_args(&event).unwrap();
        assert_eq!(input::decode_key_event_args(&body).unwrap(), event);
    }
}

#[test]
fn text_and_layout_carry_utf8() {
    for text in ["a", "é", "§", "€", ""] {
        let event = input::TextInputArgs {
            utf8: text.to_string(),
        };
        let body = input::encode_text_input_args(&event).unwrap();
        assert_eq!(input::decode_text_input_args(&body).unwrap(), event);
    }
    let layout = input::LayoutChangedArgs {
        layout: "fr".to_string(),
    };
    let body = input::encode_layout_changed_args(&layout).unwrap();
    assert_eq!(input::decode_layout_changed_args(&body).unwrap(), layout);
}

#[test]
fn keyboard_enter_carries_the_held_keys() {
    for down in [vec![], vec![0xE1], vec![0x04, 0x1A, 0xE0, 0xE1, 0xE2]] {
        let event = input::KeyboardEnterArgs { down };
        let body = input::encode_keyboard_enter_args(&event).unwrap();
        assert_eq!(input::decode_keyboard_enter_args(&body).unwrap(), event);
    }
}

#[test]
fn open_distinguishes_an_absent_surface() {
    for surface in [None, Some(0), Some(7), Some(u64::MAX)] {
        let args = input::OpenArgs { surface };
        let body = input::encode_open_args(&args).unwrap();
        assert_eq!(input::decode_open_args(&body).unwrap(), args);
    }
    let reply = input::OpenReply { session: 42 };
    let body = input::encode_open_reply(&reply).unwrap();
    assert_eq!(input::decode_open_reply(&body).unwrap(), reply);
}

#[test]
fn get_state_roundtrips() {
    let reply = input::GetStateReply {
        layout: "fr".to_string(),
        mods: 0x50,
        repeat_delay_ms: 500,
        repeat_interval_ms: 30,
    };
    let body = input::encode_get_state_reply(&reply).unwrap();
    assert_eq!(input::decode_get_state_reply(&body).unwrap(), reply);
}

#[test]
fn shell_calls_roundtrip() {
    let focus = shell::SetFocusArgs { surface: Some(9) };
    let body = shell::encode_set_focus_args(&focus).unwrap();
    assert_eq!(shell::decode_set_focus_args(&body).unwrap(), focus);
    let cleared = shell::SetFocusArgs { surface: None };
    let body = shell::encode_set_focus_args(&cleared).unwrap();
    assert_eq!(shell::decode_set_focus_args(&body).unwrap(), cleared);

    let register = shell::RegisterSurfaceArgs {
        surface: 3,
        owner: 12,
    };
    let body = shell::encode_register_surface_args(&register).unwrap();
    assert_eq!(
        shell::decode_register_surface_args(&body).unwrap(),
        register
    );

    let hotkey = shell::RegisterHotkeyArgs {
        code: 0x2B,
        mods: 4,
    };
    let body = shell::encode_register_hotkey_args(&hotkey).unwrap();
    assert_eq!(shell::decode_register_hotkey_args(&body).unwrap(), hotkey);
    let reply = shell::RegisterHotkeyReply { id: 5 };
    let body = shell::encode_register_hotkey_reply(&reply).unwrap();
    assert_eq!(shell::decode_register_hotkey_reply(&body).unwrap(), reply);
}

#[test]
fn shell_events_roundtrip() {
    let opened = shell::SessionOpenedArgs { surface: 8 };
    let body = shell::encode_session_opened_args(&opened).unwrap();
    assert_eq!(shell::decode_session_opened_args(&body).unwrap(), opened);
    let closed = shell::SessionClosedArgs { surface: 8 };
    let body = shell::encode_session_closed_args(&closed).unwrap();
    assert_eq!(shell::decode_session_closed_args(&body).unwrap(), closed);
    let fired = shell::HotkeyFiredArgs { id: 77 };
    let body = shell::encode_hotkey_fired_args(&fired).unwrap();
    assert_eq!(shell::decode_hotkey_fired_args(&body).unwrap(), fired);
}

#[test]
fn pointer_calls_and_events_roundtrip() {
    let bounds = shell::SetBoundsArgs {
        width: 1920,
        height: 1080,
    };
    let body = shell::encode_set_bounds_args(&bounds).unwrap();
    assert_eq!(shell::decode_set_bounds_args(&body).unwrap(), bounds);
    let seed = shell::GetPointerReply {
        x: 0,
        y: i32::MAX,
        buttons: 0x1F,
    };
    let body = shell::encode_get_pointer_reply(&seed).unwrap();
    assert_eq!(shell::decode_get_pointer_reply(&body).unwrap(), seed);
    for (wheel, wheel_h) in [(0, 0), (-3, 2), (i32::MIN, i32::MAX)] {
        let event = shell::PointerEventArgs {
            x: 1919,
            y: 0,
            buttons: 5,
            wheel,
            wheel_h,
            ts_ns: u64::MAX,
            seq: 1 << 40,
        };
        let body = shell::encode_pointer_event_args(&event).unwrap();
        assert_eq!(shell::decode_pointer_event_args(&body).unwrap(), event);
    }
}

/// A cut-off body is refused. An *empty* body is not: missing fields decode
/// as zero (the MIDL rule), so consumers must not act on `code == 0`. It is
/// not a key (HID usage 0 is reserved), which is why `KeyEvent` consumers
/// ignore it.
#[test]
fn truncated_bodies_are_rejected_and_empty_ones_decode_to_zero() {
    let body = input::encode_key_event_args(&input::KeyEventArgs {
        code: 4,
        sym: 97,
        mods: 0,
        state: 0,
        ts_ns: 1,
        seq: 2,
    })
    .unwrap();
    assert!(input::decode_key_event_args(&body[..body.len() - 3]).is_err());
    let empty = input::decode_key_event_args(&[]).unwrap();
    assert_eq!(empty.code, 0);
    assert_eq!(empty.state, input::KEY_STATE_DOWN);
}
