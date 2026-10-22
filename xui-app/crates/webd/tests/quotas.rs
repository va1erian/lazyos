//! What the service refuses, and that a refused request leaves nothing behind: every object it
//! carried is closed, no view is made, the engine hears nothing.

#![cfg(not(feature = "host"))]

mod support;

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{origin, rig};
use xui_webd::fabric::errno::*;
use xui_webd::limits;

#[test]
fn the_ninth_view_of_a_client_is_refused_and_another_client_is_not_affected() {
    let mut rig = rig();
    let app = origin(7);
    for _ in 0..limits::VIEWS_PER_CLIENT {
        rig.create_plain(app, 2, 2);
    }
    let handles = rig.fabric.open_handles();
    assert_eq!(
        rig.create(app, wire::LightConfig::default(), 2, 2),
        Err(ENOSPC)
    );
    // The endpoint of the refused request was closed, not kept.
    assert_eq!(rig.fabric.open_handles(), handles);
    assert!(rig
        .create(origin(8), wire::LightConfig::default(), 2, 2)
        .is_ok());
    // Destroying one frees the place.
    let (first, _) = rig
        .create(origin(9), wire::LightConfig::default(), 2, 2)
        .unwrap();
    rig.destroy(origin(9), first).unwrap();
    assert!(rig
        .create(origin(9), wire::LightConfig::default(), 2, 2)
        .is_ok());
}

#[test]
fn sizes_and_configurations_outside_the_rules_are_refused_before_the_engine_hears() {
    let mut rig = rig();
    let app = origin(7);
    let plain = wire::LightConfig::default;
    for (w, h) in [
        (0, 5),
        (5, 0),
        (8193, 1),
        (1, 8193),
        (4097, 4096),
        (u32::MAX, u32::MAX),
    ] {
        assert_eq!(rig.create(app, plain(), w, h), Err(EINVAL), "{w}x{h}");
    }
    assert!(rig.create(app, plain(), 4096, 4096).is_ok());
    assert!(rig.create(app, plain(), 8192, 2048).is_ok());
    let bad = [
        wire::LightConfig {
            color_scheme: 2,
            ..plain()
        },
        wire::LightConfig {
            default_font_size: u32::MAX,
            ..plain()
        },
        wire::LightConfig {
            user_style: "a".repeat(limits::USER_STYLE_BYTES_MAX + 1),
            ..plain()
        },
        wire::LightConfig {
            user_style: "a\0b".into(),
            ..plain()
        },
    ];
    for config in bad {
        assert_eq!(rig.create(app, config, 10, 10), Err(EINVAL));
    }
    let ok = wire::LightConfig {
        user_style: "a".repeat(limits::USER_STYLE_BYTES_MAX),
        ..plain()
    };
    assert!(rig.create(app, ok, 10, 10).is_ok());
    assert_eq!(rig.service.view_count(), 3);
    assert_eq!(
        rig.engine.calls().len(),
        3,
        "only the three good views reached the engine"
    );
}

#[test]
fn a_light_config_cannot_ask_for_more_than_its_four_fields() {
    use libmessenger::{Encoder, Kind};
    let mut rig = rig();
    let app = origin(7);
    // A hand-built request whose config carries fields only a full view has (an origin list, a
    // profile, a scheme policy): the decoder skips them and the engine's configuration stays the
    // quiet default.
    let channel = rig.fabric.new_channel();
    let mut config = Encoder::new();
    config.bool(1, false).unwrap();
    config.string(5, "https://example.com/").unwrap();
    config.string(6, "browser").unwrap();
    config.bool(7, true).unwrap();
    let mut objects = Vec::new();
    let mut body = Encoder::new();
    body.raw(Kind::Struct, 1, config.as_bytes()).unwrap();
    body.u32(2, 4).unwrap();
    body.u32(3, 4).unwrap();
    body.channel(5, channel, &mut objects).unwrap();
    rig.raw(app, wire::METHOD_CREATEVIEW, body.finish(), objects)
        .unwrap();
    let xui_webd::recording::Call::ViewCreate(_, config) = rig.engine.calls()[0].clone() else {
        panic!("no view made");
    };
    assert!(!config.scripts && config.user_style.is_empty());
}

#[test]
fn scripts_are_off_unless_the_client_asks() {
    let mut rig = rig();
    let app = origin(7);
    rig.create_plain(app, 4, 4);
    let (_, _) = rig
        .create(
            app,
            wire::LightConfig {
                scripts: true,
                ..Default::default()
            },
            4,
            4,
        )
        .unwrap();
    let flags: Vec<bool> = rig
        .engine
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            xui_webd::recording::Call::ViewCreate(_, config) => Some(config.scripts),
            _ => None,
        })
        .collect();
    assert_eq!(flags, vec![false, true]);
}

#[test]
fn bad_base_urls_and_oversized_pages_are_refused_and_the_buffer_is_closed() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    let base = rig.fabric.open_handles();
    for url in [
        "http://example.com/",
        "file:///etc/passwd",
        "https://x/",
        "blob:null/1",
        "relative",
    ] {
        assert_eq!(
            rig.load_html(app, view, b"<p>x</p>", url),
            Err(EINVAL),
            "{url}"
        );
    }
    for url in ["", "about:blank", "data:text/html,x"] {
        assert!(rig.load_html(app, view, b"<p>x</p>", url).is_ok(), "{url}");
    }
    // A range over the cap is refused on its declared length, before anything is mapped.
    let (handle, _) = rig.fabric.new_buffer(16);
    let huge = Buffer::whole(handle, limits::HTML_BYTES_MAX + 1);
    assert_eq!(rig.load_buffer(app, view, huge, "about:blank"), Err(E2BIG));
    // A range past the end of the real buffer is refused too.
    let (handle, _) = rig.fabric.new_buffer(16);
    assert_eq!(
        rig.load_buffer(
            app,
            view,
            Buffer {
                handle,
                offset: 8,
                len: 16
            },
            ""
        ),
        Err(EINVAL)
    );
    assert_eq!(rig.fabric.open_handles(), base);
    assert_eq!(rig.fabric.double_closes(), 0);
    // Only the three good pages reached the engine.
    let loads = rig
        .engine
        .calls()
        .iter()
        .filter(|call| matches!(call, xui_webd::recording::Call::LoadHtml { .. }))
        .count();
    assert_eq!(loads, 3);
}

#[test]
fn html_at_the_cap_and_a_range_inside_a_bigger_buffer_load() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    let (handle, _) = rig.fabric.new_buffer_with(b"xx<b>hi</b>yy".to_vec());
    let part = Buffer {
        handle,
        offset: 2,
        len: 9,
    };
    rig.load_buffer(app, view, part, "").unwrap();
    assert_eq!(
        rig.engine.calls().last(),
        Some(&xui_webd::recording::Call::LoadHtml {
            view,
            html: b"<b>hi</b>".to_vec(),
            base_url: "about:blank".into()
        })
    );
    // Invalid UTF-8 becomes U+FFFD instead of a refused page.
    rig.load_html(app, view, &[b'a', 0xFF, b'b'], "").unwrap();
    let Some(xui_webd::recording::Call::LoadHtml { html, .. }) = rig.engine.calls().last().cloned()
    else {
        panic!()
    };
    assert_eq!(String::from_utf8(html).unwrap(), "a\u{FFFD}b");
}

#[test]
fn slots_must_fit_the_view_and_the_slot_number() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    let base = rig.fabric.open_handles();
    let (small, _) = rig.attach_bytes(app, view, 0, 4 * 4 * 4 - 1);
    assert_eq!(small, Err(EINVAL));
    let (out_of_range, _) = rig.attach_bytes(app, view, limits::SLOTS_PER_VIEW as u32, 64);
    assert_eq!(out_of_range, Err(EINVAL));
    // A range that does not fit the real buffer.
    let (handle, _) = rig.fabric.new_buffer(32);
    assert_eq!(rig.attach_handle(app, view, 0, handle, 64), Err(EINVAL));
    // An overflowing range.
    let (handle, _) = rig.fabric.new_buffer(64);
    let args = wire::AttachSlotArgs {
        view,
        slot: 0,
        pixels: Buffer {
            handle,
            offset: u64::MAX,
            len: 64,
        },
    };
    let (body, objects) = wire::encode_attach_slot_args(&args).unwrap();
    assert_eq!(
        rig.raw(app, wire::METHOD_ATTACHSLOT, body, objects),
        Err(EINVAL)
    );
    assert_eq!(
        rig.fabric.open_handles(),
        base,
        "every refused buffer was closed"
    );
    assert!(rig.attach_bytes(app, view, 2, 64).0.is_ok());
}

#[test]
fn a_client_may_hold_512_mib_of_slots_and_not_a_byte_more() {
    let mut rig = rig();
    let app = origin(7);
    let slot_bytes = 4096u64 * 4096 * 4;
    assert_eq!(limits::SLOT_BYTES_PER_CLIENT, 8 * slot_bytes);
    // One 64 MiB region shared by every handle: the fabric maps ranges, not copies.
    let (_, region) = rig.fabric.new_buffer(slot_bytes as usize);
    let attach = |rig: &mut support::Rig, view, slot| {
        let handle = rig.fabric.new_buffer_shared(region.clone());
        rig.attach_handle(app, view, slot, handle, slot_bytes)
    };
    let (a, _) = rig.create_plain(app, 4096, 4096);
    let (b, _) = rig.create_plain(app, 4096, 4096);
    let (c, _) = rig.create_plain(app, 4096, 4096);
    for slot in 0..3 {
        assert!(attach(&mut rig, a, slot).is_ok());
        assert!(attach(&mut rig, b, slot).is_ok());
    }
    assert!(attach(&mut rig, c, 0).is_ok());
    assert!(attach(&mut rig, c, 1).is_ok(), "exactly 512 MiB");
    assert_eq!(attach(&mut rig, c, 2), Err(EDQUOT));
    // Replacing a free slot counts the difference, not the sum.
    assert!(attach(&mut rig, c, 1).is_ok());
    // Another client has its own allowance.
    let (d, _) = rig.create_plain(origin(8), 4096, 4096);
    let other = rig.fabric.new_buffer_shared(region.clone());
    assert!(rig
        .attach_handle(origin(8), d, 0, other, slot_bytes)
        .is_ok());
    // Ending a view returns its share.
    rig.destroy(app, a).unwrap();
    assert!(attach(&mut rig, c, 2).is_ok());
}

#[test]
fn a_buffer_is_charged_in_full_whatever_range_the_client_declares_of_it() {
    let mut rig = rig();
    let app = origin(7);
    let slot_bytes = 4096u64 * 4096 * 4;
    // Each buffer is 64 MiB and declares 64 bytes (a 4x4 view): the old charge was 64 bytes, so
    // all 24 slots fitted; the kernel maps all 64 MiB of each one, so the ninth does not.
    let (_, region) = rig.fabric.new_buffer(slot_bytes as usize);
    let views: Vec<u64> = (0..limits::VIEWS_PER_CLIENT)
        .map(|_| rig.create_plain(app, 4, 4).0)
        .collect();
    let mut attached = 0u64;
    let mut refused = None;
    'all: for slot in 0..limits::SLOTS_PER_VIEW as u32 {
        for view in &views {
            let handle = rig.fabric.new_buffer_shared(region.clone());
            match rig.attach_handle(app, *view, slot, handle, 64) {
                Ok(_) => attached += 1,
                Err(code) => {
                    refused = Some(code);
                    break 'all;
                }
            }
        }
    }
    assert_eq!(attached * slot_bytes, limits::SLOT_BYTES_PER_CLIENT);
    assert_eq!(refused, Some(EDQUOT));
    // The refused buffer was closed, not kept; destroying everything closes the rest (what stays
    // open is the test's own handle on the region).
    for view in views {
        rig.destroy(app, view).unwrap();
    }
    assert_eq!(rig.fabric.open_handles(), 1);
    assert_eq!(rig.fabric.double_closes(), 0);
}

#[test]
fn a_declared_length_near_u64_max_is_refused_without_wrapping_the_quota_sum() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    // Something is already charged, so `used + len` would wrap in the old arithmetic.
    rig.attach(app, view, 0, 4, 4);
    let base = rig.fabric.open_handles();
    for len in [u64::MAX, u64::MAX - 1, u64::MAX - 64, 1 << 63] {
        let (handle, _) = rig.fabric.new_buffer(64);
        assert!(
            rig.attach_handle(app, view, 1, handle, len).is_err(),
            "{len}"
        );
    }
    assert_eq!(
        rig.fabric.open_handles(),
        base,
        "every refused buffer was closed"
    );
    assert_eq!(rig.fabric.double_closes(), 0);
    // The client's honest slot is untouched and the next honest attach works.
    assert!(rig.attach_bytes(app, view, 1, 64).0.is_ok());
}

#[test]
fn unknown_methods_wrong_interfaces_and_other_sessions_are_refused() {
    use libmessenger::Object;
    let mut rig = rig();
    let app = origin(7);
    assert_eq!(rig.raw(app, 4242, vec![], vec![]), Err(ENOTSUP));
    // A different interface id.
    let call = xui_webd::Call {
        origin: app,
        parcel: support::parcel(1234, wire::METHOD_DESTROYVIEW, vec![]),
        objects: vec![],
    };
    assert_eq!(support::outcome(&rig.service.handle(call)), Err(ENOTSUP));
    // Another login session than webd's.
    let mut stranger = app;
    stranger.session = support::SESSION + 1;
    let channel = rig.fabric.new_channel();
    let (body, objects) = wire::encode_create_view_args(&wire::CreateViewArgs {
        config: Default::default(),
        width: 2,
        height: 2,
        events: channel,
    })
    .unwrap();
    assert_eq!(
        rig.raw(stranger, wire::METHOD_CREATEVIEW, body, objects),
        Err(EACCES)
    );
    assert!(
        !rig.fabric.channel_open(channel),
        "the refused endpoint is closed"
    );
    assert_eq!(rig.service.view_count(), 0);
    // Garbage and a wrong object list are malformed, and nothing leaks.
    let channel = rig.fabric.new_channel();
    assert_eq!(
        rig.raw(
            app,
            wire::METHOD_CREATEVIEW,
            vec![1, 2, 3],
            vec![Object::Channel(channel)]
        ),
        Err(EINVAL)
    );
    assert!(!rig.fabric.channel_open(channel));
    assert_eq!(rig.fabric.open_handles(), 0);
    assert_eq!(rig.fabric.double_closes(), 0);
}

#[test]
fn bad_input_is_dropped_not_forwarded() {
    let mut rig = rig();
    let app = origin(7);
    let (view, _) = rig.create_plain(app, 4, 4);
    rig.engine.take();
    let mouse = |kind, button| {
        wire::encode_mouse_args(&wire::MouseArgs {
            view,
            kind,
            button,
            x: 1,
            y: 1,
            mods: 0,
        })
        .unwrap()
    };
    assert_eq!(
        rig.raw(app, wire::METHOD_MOUSE, mouse(3, 0), vec![]),
        Err(EINVAL)
    );
    assert_eq!(
        rig.raw(app, wire::METHOD_MOUSE, mouse(0, 3), vec![]),
        Err(EINVAL)
    );
    assert_eq!(
        rig.raw(app, wire::METHOD_MOUSE, mouse(0, -2), vec![]),
        Err(EINVAL)
    );
    // -1 is "no button" (a move with nothing held): forwarded as it is.
    assert_eq!(
        rig.raw(app, wire::METHOD_MOUSE, mouse(2, -1), vec![]),
        Ok(Vec::new())
    );
    let key = wire::encode_key_args(&wire::KeyArgs {
        view,
        press: true,
        keysym: 1,
        text: "a\0b".into(),
        mods: 0,
    })
    .unwrap();
    assert_eq!(rig.raw(app, wire::METHOD_KEY, key, vec![]), Err(EINVAL));
    // Only the move with no button reached the engine.
    assert_eq!(rig.engine.take().len(), 1);
}
