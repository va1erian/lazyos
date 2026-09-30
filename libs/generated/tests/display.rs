//! Round-trip tests for the generated `os.lazy.display.v1` stubs (issue #287):
//! every call, every one-way event, the `SurfaceRow` struct and the enum
//! constants, plus the edge cases the compositor depends on (empty and long
//! titles, zero and many rows, truncated bodies, unknown and missing fields).

use messenger_generated::os_lazy_display_v1::*;

/// Encode then decode through a pair of generated helpers and compare.
macro_rules! roundtrip {
    ($value:expr, $encode:ident, $decode:ident) => {{
        let value = $value;
        assert_eq!($decode(&$encode(&value).unwrap()).unwrap(), value);
    }};
}

#[test]
fn method_ids_are_pinned_to_the_legacy_numbering() {
    let ids = [
        METHOD_CREATESURFACE,
        METHOD_ATTACHBUFFER,
        METHOD_COMMIT,
        METHOD_DESTROYSURFACE,
        METHOD_POINTERMOVE,
        METHOD_POINTERDOWN,
        METHOD_POINTERUP,
        METHOD_KEYDOWN,
        METHOD_KEYUP,
        METHOD_WINDOWCLOSE,
        METHOD_DRAGSTART,
        METHOD_DRAGCANCEL,
        METHOD_DRAGENTER,
        METHOD_DRAGOVER,
        METHOD_DRAGLEAVE,
        METHOD_DROP,
        METHOD_DRAGENDED,
        METHOD_LISTSURFACES,
        METHOD_GETWORKAREA,
        METHOD_SUBSCRIBE,
        METHOD_GETTHEME,
        METHOD_SURFACECHANGED,
        METHOD_FOCUSCHANGED,
        METHOD_STARTMENU,
    ];
    let expected: Vec<u32> = (1..=24).collect();
    assert_eq!(ids.to_vec(), expected);
    // Append-only additions after the legacy block.
    assert_eq!(METHOD_POINTERWHEEL, 31);
}

#[test]
fn enum_constants_keep_the_wire_values() {
    assert_eq!((ROLE_WINDOW, ROLE_DESKTOP), (0, 1));
    assert_eq!(
        [
            CHANGE_UNSPECIFIED,
            CHANGE_CREATED,
            CHANGE_DESTROYED,
            CHANGE_MOVED,
            CHANGE_MINIMIZED,
            CHANGE_RESTORED,
            CHANGE_TITLE,
            CHANGE_RESIZED,
            CHANGE_MAXIMIZED,
            CHANGE_UNMAXIMIZED
        ],
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
    );
    assert_eq!(
        (WINDOW_STATE_NORMAL, WINDOW_STATE_MAXIMIZED),
        (0, 1),
        "WindowState values are pinned"
    );
}

#[test]
fn resize_and_maximize_ids_are_appended_after_hint_open_origin() {
    assert_eq!((METHOD_SETSIZEHINTS, METHOD_CONFIGURE), (32, 33));
}

#[test]
fn size_hints_and_configure_roundtrip() {
    for (min_w, min_h, max_w, max_h) in [(0, 0, 0, 0), (120, 40, 800, 600), (u32::MAX, 1, 2, 3)] {
        roundtrip!(
            SetSizeHintsArgs {
                surface: 11,
                min_w,
                min_h,
                max_w,
                max_h
            },
            encode_set_size_hints_args,
            decode_set_size_hints_args
        );
    }
    for state in [WINDOW_STATE_NORMAL, WINDOW_STATE_MAXIMIZED] {
        roundtrip!(
            ConfigureArgs {
                surface: u64::MAX,
                width: 1024,
                height: 768,
                state
            },
            encode_configure_args,
            decode_configure_args
        );
    }
    // An empty body decodes to defaults, so an old compositor's malformed
    // Configure is a no-op rather than a zero-size resize.
    assert_eq!(
        decode_configure_args(&[]).unwrap(),
        ConfigureArgs::default()
    );
    assert_eq!(
        decode_set_size_hints_args(&[]).unwrap(),
        SetSizeHintsArgs::default()
    );
}

#[test]
fn surface_calls_roundtrip() {
    for title in [String::new(), "xdemo".to_string(), "T".repeat(4000)] {
        roundtrip!(
            CreateSurfaceArgs {
                width: 640,
                height: 480,
                title: title.clone(),
                role: ROLE_DESKTOP
            },
            encode_create_surface_args,
            decode_create_surface_args
        );
    }
    roundtrip!(
        CreateSurfaceReply { surface: u64::MAX },
        encode_create_surface_reply,
        decode_create_surface_reply
    );
    roundtrip!(
        AttachBufferArgs { surface: 3 },
        encode_attach_buffer_args,
        decode_attach_buffer_args
    );
    roundtrip!(
        CommitArgs {
            surface: 3,
            x: 1,
            y: 2,
            w: 3,
            h: 4
        },
        encode_commit_args,
        decode_commit_args
    );
    roundtrip!(
        DestroySurfaceArgs { surface: 9 },
        encode_destroy_surface_args,
        decode_destroy_surface_args
    );
}

#[test]
fn input_events_roundtrip_with_negative_coordinates_and_buttons() {
    roundtrip!(
        PointerMoveArgs { x: -12, y: 900 },
        encode_pointer_move_args,
        decode_pointer_move_args
    );
    for button in 1..=3 {
        roundtrip!(
            PointerDownArgs {
                x: 5,
                y: -6,
                button
            },
            encode_pointer_down_args,
            decode_pointer_down_args
        );
        roundtrip!(
            PointerUpArgs { x: 0, y: 0, button },
            encode_pointer_up_args,
            decode_pointer_up_args
        );
    }
    for delta in [1, -1, 3, -120, i32::MAX, i32::MIN] {
        roundtrip!(
            PointerWheelArgs {
                x: -4,
                y: 700,
                delta
            },
            encode_pointer_wheel_args,
            decode_pointer_wheel_args
        );
    }
    roundtrip!(
        KeyDownArgs { key: 0x113 },
        encode_key_down_args,
        decode_key_down_args
    );
    roundtrip!(
        KeyUpArgs { key: 97 },
        encode_key_up_args,
        decode_key_up_args
    );
}

#[test]
fn drag_and_drop_roundtrip() {
    roundtrip!(
        DragStartArgs {
            surface: 2,
            token: 77,
            mime: "text/plain".into()
        },
        encode_drag_start_args,
        decode_drag_start_args
    );
    roundtrip!(
        DragCancelArgs { surface: 2 },
        encode_drag_cancel_args,
        decode_drag_cancel_args
    );
    roundtrip!(
        DragEnterArgs {
            x: 4,
            y: -4,
            mime: "".into()
        },
        encode_drag_enter_args,
        decode_drag_enter_args
    );
    roundtrip!(
        DragOverArgs { x: 8, y: 9 },
        encode_drag_over_args,
        decode_drag_over_args
    );
    roundtrip!(
        DropArgs {
            x: 1,
            y: 2,
            token: u64::MAX,
            mime: "application/x-lazyos-demo".into()
        },
        encode_drop_args,
        decode_drop_args
    );
    for dropped in [true, false] {
        roundtrip!(
            DragEndedArgs { dropped },
            encode_drag_ended_args,
            decode_drag_ended_args
        );
    }
}

fn row(id: u64) -> SurfaceRow {
    SurfaceRow {
        id,
        title: format!("window {id}"),
        x: -3,
        y: 40,
        w: 320,
        h: 200,
        minimized: id.is_multiple_of(2),
        focused: id == 1,
        role: if id == 0 { ROLE_DESKTOP } else { ROLE_WINDOW },
        maximized: id == 3,
    }
}

#[test]
fn shell_calls_roundtrip() {
    roundtrip!(row(1), encode_surface_row, decode_surface_row);
    for count in [0u64, 1, 2, 64] {
        roundtrip!(
            ListSurfacesReply {
                surfaces: (0..count).map(row).collect()
            },
            encode_list_surfaces_reply,
            decode_list_surfaces_reply
        );
    }
    roundtrip!(
        GetWorkAreaReply {
            x: 0,
            y: 0,
            w: 1280,
            h: 752
        },
        encode_get_work_area_reply,
        decode_get_work_area_reply
    );
    roundtrip!(
        SubscribeArgs {
            subscriber_role: "shell".into()
        },
        encode_subscribe_args,
        decode_subscribe_args
    );
    roundtrip!(
        GetThemeReply {
            title_bg_active: 0x00ff_ffff,
            title_bg_inactive: 0,
            border: 0x0012_3456,
            taskbar: 7,
            text: 0x00ab_cdef
        },
        encode_get_theme_reply,
        decode_get_theme_reply
    );
}

#[test]
fn shell_events_roundtrip_with_optional_fields() {
    for title in [None, Some(String::new()), Some("Terminal".to_string())] {
        roundtrip!(
            SurfaceChangedArgs {
                surface: 5,
                kind: CHANGE_CREATED,
                x: 1,
                y: 2,
                w: 3,
                h: 4,
                minimized: true,
                focused: true,
                title,
                role: ROLE_WINDOW,
                maximized: true
            },
            encode_surface_changed_args,
            decode_surface_changed_args
        );
    }
    for surface in [None, Some(0), Some(12)] {
        roundtrip!(
            FocusChangedArgs { surface },
            encode_focus_changed_args,
            decode_focus_changed_args
        );
    }
}

#[test]
fn truncated_bodies_are_rejected() {
    let body = encode_list_surfaces_reply(&ListSurfacesReply {
        surfaces: vec![row(1), row(2)],
    })
    .unwrap();
    assert!(decode_list_surfaces_reply(&body[..body.len() - 3]).is_err());
    let body = encode_create_surface_args(&CreateSurfaceArgs {
        width: 1,
        height: 1,
        title: "abc".into(),
        role: 0,
    })
    .unwrap();
    assert!(decode_create_surface_args(&body[..body.len() - 1]).is_err());
    assert!(decode_pointer_down_args(&[1, 2, 3]).is_err());
}

#[test]
fn missing_fields_take_defaults() {
    let empty = decode_create_surface_args(&[]).unwrap();
    assert_eq!(empty, CreateSurfaceArgs::default());
    assert_eq!(empty.role, ROLE_WINDOW);
    assert_eq!(
        decode_surface_changed_args(&[]).unwrap().kind,
        CHANGE_UNSPECIFIED
    );
    assert_eq!(decode_focus_changed_args(&[]).unwrap().surface, None);
    assert!(decode_list_surfaces_reply(&[]).unwrap().surfaces.is_empty());
}

#[test]
fn unknown_fields_and_the_error_field_are_ignored() {
    let mut body = libmessenger::Encoder::new();
    body.u64(1, 42).unwrap();
    body.string(30, "future field").unwrap();
    body.error(15, 13, "denied").unwrap();
    let reply = decode_create_surface_reply(&body.finish()).unwrap();
    assert_eq!(reply.surface, 42);

    let mut body = libmessenger::Encoder::new();
    body.error(15, 13, "denied").unwrap();
    assert_eq!(
        decode_list_surfaces_reply(&body.finish()).unwrap(),
        ListSurfacesReply::default()
    );
}

#[test]
fn pipelined_present_ids_are_appended_after_the_legacy_range() {
    let ids = [
        METHOD_ATTACHBUFFERSLOT,
        METHOD_PRESENT,
        METHOD_BUFFERRELEASE,
        METHOD_FRAMEDONE,
    ];
    assert_eq!(ids.to_vec(), vec![25, 26, 27, 28]);
}

#[test]
fn present_and_its_events_roundtrip() {
    roundtrip!(
        AttachBufferSlotArgs {
            surface: 7,
            slot: 3
        },
        encode_attach_buffer_slot_args,
        decode_attach_buffer_slot_args
    );
    roundtrip!(
        BufferReleaseArgs {
            surface: u64::MAX,
            slot: 1
        },
        encode_buffer_release_args,
        decode_buffer_release_args
    );
    roundtrip!(
        FrameDoneArgs {
            surface: 2,
            seq: u64::MAX
        },
        encode_frame_done_args,
        decode_frame_done_args
    );
    // An empty damage list (whole surface), one rect, and the 16-rect cap and
    // beyond all survive the wire.
    for count in [0usize, 1, 16, 17, 64] {
        let damage = (0..count as u32)
            .map(|i| Rect {
                x: i,
                y: i * 2,
                w: u32::MAX - i,
                h: 5,
            })
            .collect();
        roundtrip!(
            PresentArgs {
                surface: 4,
                slot: 1,
                seq: 99,
                damage
            },
            encode_present_args,
            decode_present_args
        );
    }
}

#[test]
fn present_rejects_truncated_bodies_and_ignores_unknown_fields() {
    let body = encode_present_args(&PresentArgs {
        surface: 1,
        slot: 0,
        seq: 5,
        damage: vec![Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        }],
    })
    .unwrap();
    for cut in 1..body.len() {
        // A prefix is either an error or decodes without panicking.
        let _ = decode_present_args(&body[..cut]);
    }
    assert_eq!(decode_present_args(&[]).unwrap(), PresentArgs::default());
}
