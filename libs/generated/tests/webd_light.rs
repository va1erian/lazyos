//! Round-trip tests for the light view stubs (docs/webd-plan.md section 4): `os.lazy.webd.light.v1`,
//! its object slots and the wire values the Rust and C halves of webd agree on.

use libmessenger::{Buffer, Error, Object};
use messenger_generated::os_lazy_webd_light_v1::*;

#[test]
fn ids_are_append_only_from_the_first_release() {
    // These numbers are on the wire; the full interface reuses them.
    let ids = [
        (METHOD_CREATEVIEW, 1),
        (METHOD_DESTROYVIEW, 2),
        (METHOD_ATTACHSLOT, 3),
        (METHOD_RELEASESLOT, 4),
        (METHOD_RESIZE, 5),
        (METHOD_SETVISIBLE, 6),
        (METHOD_REFRESH, 7),
        (METHOD_LOADHTML, 11),
        (METHOD_MOUSE, 20),
        (METHOD_WHEEL, 21),
        (METHOD_KEY, 22),
        (METHOD_FOCUS, 23),
        (METHOD_FRAME, 100),
        (METHOD_STATE, 101),
        (METHOD_SCROLL, 102),
        (METHOD_LINKCLICKED, 103),
        (METHOD_CLIPBOARDWRITE, 104),
        (METHOD_VIEWLOST, 105),
        (METHOD_PING, 106),
    ];
    for (got, want) in ids {
        assert_eq!(got, want);
    }
    assert_eq!(INTERFACE_NAME, "os.lazy.webd.light.v1");
    // The enums travel as numbers; the C ABI's `WEBD_LOST_*` are 1 to 4 and 0 is "unknown".
    assert_eq!(
        [
            LOST_REASON_UNKNOWN,
            LOST_REASON_REFUSED,
            LOST_REASON_CRASHED,
            LOST_REASON_OUT_OF_MEMORY,
            LOST_REASON_OTHER
        ],
        [0, 1, 2, 3, 4]
    );
    assert_eq!((COLOR_SCHEME_AUTO, COLOR_SCHEME_DARK), (0, 1));
    assert_eq!(
        [MOUSE_KIND_MOVE, MOUSE_KIND_PRESS, MOUSE_KIND_RELEASE],
        [0, 1, 2]
    );
}

#[test]
fn create_view_carries_the_config_and_exactly_one_channel() {
    let args = CreateViewArgs {
        config: LightConfig {
            scripts: true,
            color_scheme: COLOR_SCHEME_DARK,
            default_font_size: 18,
            user_style: "p { margin: 0 }".into(),
        },
        width: 640,
        height: 480,
        events: 11,
    };
    let (body, objects) = encode_create_view_args(&args).unwrap();
    assert_eq!(objects, vec![Object::Channel(11)]);
    // The kernel installs its own handle numbers; the decoder returns those.
    let installed = [Object::Channel(77)];
    let decoded = decode_create_view_args(&body, &installed).unwrap();
    assert_eq!(decoded, CreateViewArgs { events: 77, ..args });
    // Missing, extra or wrong-kind objects are refused.
    assert_eq!(
        decode_create_view_args(&body, &[]),
        Err(Error::BadObjectIndex)
    );
    assert_eq!(
        decode_create_view_args(&body, &[Object::Buffer(1)]),
        Err(Error::BadObjectIndex)
    );
    assert_eq!(
        decode_create_view_args(&body, &[Object::Channel(1), Object::Channel(2)]),
        Err(Error::BadObjectIndex)
    );
    let reply = CreateViewReply { view: u64::MAX };
    let body = encode_create_view_reply(&reply).unwrap();
    assert_eq!(decode_create_view_reply(&body).unwrap(), reply);
    assert_eq!(CREATE_VIEW_OBJECTS.len(), 1);
}

#[test]
fn a_default_config_is_four_quiet_fields() {
    let config = LightConfig::default();
    assert!(!config.scripts && config.user_style.is_empty());
    let (body, _) = encode_create_view_args(&CreateViewArgs {
        config: config.clone(),
        width: 1,
        height: 1,
        events: 1,
    })
    .unwrap();
    let decoded = decode_create_view_args(&body, &[Object::Channel(2)]).unwrap();
    assert_eq!(decoded.config, config);
}

#[test]
fn slots_and_pages_travel_as_buffers() {
    let attach = AttachSlotArgs {
        view: 3,
        slot: 2,
        pixels: Buffer::whole(5, 4096),
    };
    let (body, objects) = encode_attach_slot_args(&attach).unwrap();
    assert_eq!(objects, vec![Object::Buffer(5)]);
    let decoded = decode_attach_slot_args(&body, &[Object::Buffer(9)]).unwrap();
    assert_eq!(decoded.pixels, Buffer::whole(9, 4096));
    assert_eq!((decoded.view, decoded.slot), (3, 2));
    assert_eq!(
        decode_attach_slot_args(&body, &[Object::Channel(9)]),
        Err(Error::BadObjectIndex)
    );

    let load = LoadHtmlArgs {
        view: 3,
        html: Buffer {
            handle: 6,
            offset: 16,
            len: 100,
        },
        base_url: "about:blank".into(),
    };
    let (body, objects) = encode_load_html_args(&load).unwrap();
    assert_eq!(objects, vec![Object::Buffer(6)]);
    let decoded = decode_load_html_args(&body, &[Object::Buffer(4)]).unwrap();
    assert_eq!(decoded.html.offset, 16);
    assert_eq!((decoded.html.handle, decoded.html.len), (4, 100));
    assert_eq!(decoded.base_url, "about:blank");
}

#[test]
fn input_and_events_roundtrip() {
    let mouse = MouseArgs {
        view: 1,
        kind: MOUSE_KIND_PRESS,
        button: 2,
        x: -5,
        y: 700,
        mods: 0b1010,
    };
    assert_eq!(
        decode_mouse_args(&encode_mouse_args(&mouse).unwrap()).unwrap(),
        mouse
    );
    let wheel = WheelArgs {
        view: 1,
        x: 3,
        y: 4,
        ticks_x: -1,
        ticks_y: 2,
        mods: 0,
        seq: 9,
    };
    assert_eq!(
        decode_wheel_args(&encode_wheel_args(&wheel).unwrap()).unwrap(),
        wheel
    );
    let key = KeyArgs {
        view: 1,
        press: true,
        keysym: 0xff0d,
        text: "\n".into(),
        mods: 4,
    };
    assert_eq!(
        decode_key_args(&encode_key_args(&key).unwrap()).unwrap(),
        key
    );
    let frame = FrameArgs {
        view: 9,
        slot: 1,
        seq: u64::MAX,
        width: 1920,
        height: 1080,
    };
    assert_eq!(
        decode_frame_args(&encode_frame_args(&frame).unwrap()).unwrap(),
        frame
    );
    let state = StateArgs {
        view: 9,
        title: "Résumé".into(),
        loading: true,
        finished: 3,
        error: "load failed".into(),
    };
    assert_eq!(
        decode_state_args(&encode_state_args(&state).unwrap()).unwrap(),
        state
    );
    let scroll = ScrollArgs {
        view: 9,
        x: 0,
        y: -10,
        at_bottom: true,
    };
    assert_eq!(
        decode_scroll_args(&encode_scroll_args(&scroll).unwrap()).unwrap(),
        scroll
    );
    let lost = ViewLostArgs {
        view: 9,
        reason: LOST_REASON_CRASHED,
    };
    assert_eq!(
        decode_view_lost_args(&encode_view_lost_args(&lost).unwrap()).unwrap(),
        lost
    );
    let link = LinkClickedArgs {
        view: 9,
        url: "https://example.com/a?b=c".into(),
    };
    assert_eq!(
        decode_link_clicked_args(&encode_link_clicked_args(&link).unwrap()).unwrap(),
        link
    );
}

#[test]
fn only_buffers_and_the_event_channel_are_objects() {
    let declared: Vec<u32> = DECLARED_OBJECTS.iter().map(|(method, _)| *method).collect();
    assert_eq!(
        declared,
        vec![METHOD_CREATEVIEW, METHOD_ATTACHSLOT, METHOD_LOADHTML]
    );
}
