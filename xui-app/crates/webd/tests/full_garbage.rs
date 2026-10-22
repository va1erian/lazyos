//! Seeded fuzz (`FUZZ_CASES`, `FUZZ_SEED`) of the full interface's request decoders: random and
//! mutated bodies on the view interface, both scopes and the light interface, with object lists that
//! sometimes do not match. Nothing may panic, and every handle a request carried must be closed
//! (a view the request made is closed at shutdown).

#![cfg(not(feature = "host"))]

mod support;

use fuzzkit::{for_seeds, Rng};
use libmessenger::{Buffer, Object};
use messenger_generated::os_lazy_webd_light_v1 as light;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, Fx, Via};
use support::origin;
use xui_webd::fabric::Fabric;

/// Every method of the full interface, plus ones nobody defined.
const METHODS: [u32; 38] = [
    1, 2, 3, 4, 5, 6, 7, 10, 11, 12, 13, 14, 15, 16, 20, 21, 22, 23, 30, 31, 32, 33, 34, 35, 36,
    37, 100, 101, 106, 111, 112, 0, 8, 9, 17, 38, 99, 4242,
];

fn blob(rng: &mut Rng, max: u64) -> Vec<u8> {
    let len = rng.below(max) as usize;
    rng.bytes(len)
}

fn text(rng: &mut Rng) -> String {
    let options = [
        "",
        "easy",
        "a b",
        "about:blank",
        "http://10.0.2.2:18081/",
        "https://example.com",
        "file:///etc/passwd",
        "a\0b",
    ];
    (*rng.pick(&options)).to_owned()
}

fn everything() -> wire::ViewConfig {
    wire::ViewConfig {
        downloads: true,
        blocking: true,
        notifications: true,
        dialogs: true,
        deltas: true,
        popups: true,
        ..browser()
    }
}

#[test]
fn random_and_mutated_full_requests_never_panic_and_never_leak_a_handle() {
    for_seeds("webd_full_decode", |_, rng| {
        let mut fx = Fx::new("fuzz-garbage");
        let who = [origin(7), origin(8)];
        // Real views, so valid-looking requests get past the lookup.
        let (a, _) = fx.view(who[0], Via::Browser, everything(), 4, 4);
        let (b, _) = fx.view(
            who[1],
            Via::Net,
            wire::ViewConfig {
                deltas: true,
                ..support::full::web()
            },
            4,
            4,
        );
        let views = [a, b];
        let interfaces = [
            wire::INTERFACE_ID,
            messenger_generated::os_lazy_webd_net_v1::INTERFACE_ID,
            messenger_generated::os_lazy_webd_browser_v1::INTERFACE_ID,
            light::INTERFACE_ID,
        ];
        for step in 0..300 {
            let method = *rng.pick(&METHODS);
            let view = *rng.pick(&views);
            let (mut body, mut objects) = valid_request(rng, &mut fx, method, view);
            match rng.below(4) {
                0 => body = blob(rng, 80),
                1 => {
                    let flips = 1 + rng.below(4) as usize;
                    rng.flip_bits(&mut body, flips);
                }
                2 => body.truncate(rng.below(body.len() as u64 + 1) as usize),
                _ => {}
            }
            // Sometimes the object list does not match the declaration.
            match rng.below(6) {
                0 => objects.drain(..).for_each(|object| close(&fx, object)),
                1 => objects.push(Object::Buffer(fx.fabric.new_buffer(8).0)),
                2 => objects.push(Object::Channel(fx.fabric.new_channel())),
                _ => {}
            }
            let from = *rng.pick(&who);
            let interface = *rng.pick(&interfaces);
            let _ = fx.send(from, interface, method, body, objects);
            assert_eq!(fx.fabric.double_closes(), 0, "step {step}");
            if rng.one_in(10) {
                fx.pump(step * 700);
            }
        }
        fx.service.shutdown();
        assert_eq!(fx.service.view_count(), 0);
        assert_eq!(fx.fabric.open_handles(), 0, "a handle leaked");
    });
}

fn close(fx: &Fx, object: Object) {
    match object {
        Object::Channel(handle) => fx.fabric.close_channel(handle),
        Object::Buffer(handle) => fx.fabric.close_buffer(handle),
    }
}

/// A well-formed request for `method` with fresh objects, filled with values that are sometimes
/// right and sometimes not.
fn valid_request(rng: &mut Rng, fx: &mut Fx, method: u32, view: u64) -> (Vec<u8>, Vec<Object>) {
    let plain = |body: Result<Vec<u8>, libmessenger::Error>| (body.unwrap(), Vec::new());
    let request = rng.below(5);
    match method {
        wire::METHOD_CREATEVIEW => {
            let events = fx.fabric.new_channel();
            let config = wire::ViewConfig {
                profile: ["", "browser", "x"][rng.below(3) as usize].into(),
                scheme_policy: rng.below(4) as u32,
                handoff: rng.below(4) as u32,
                origins: vec![text(rng)],
                downloads: rng.one_in(3),
                blocking: rng.one_in(3),
                notifications: rng.one_in(3),
                deltas: rng.one_in(2),
                ephemeral: rng.one_in(3),
                user_agent_suffix: text(rng),
                ..wire::ViewConfig::default()
            };
            let (width, height) = (rng.below(9000) as u32, rng.below(9000) as u32);
            let args = wire::CreateViewArgs {
                config,
                width,
                height,
                events,
            };
            wire::encode_create_view_args(&args).unwrap()
        }
        wire::METHOD_DESTROYVIEW if rng.one_in(8) => {
            plain(wire::encode_destroy_view_args(&wire::DestroyViewArgs {
                view,
            }))
        }
        wire::METHOD_ATTACHSLOT => {
            let (handle, _) = fx.fabric.new_buffer(rng.below(70_000) as usize);
            let pixels = Buffer {
                handle,
                offset: rng.below(8),
                len: rng.below(70_000),
            };
            let slot = rng.below(6) as u32;
            wire::encode_attach_slot_args(&wire::AttachSlotArgs { view, slot, pixels }).unwrap()
        }
        wire::METHOD_RELEASESLOT => plain(wire::encode_release_slot_args(&wire::ReleaseSlotArgs {
            view,
            slot: rng.below(6) as u32,
        })),
        wire::METHOD_RESIZE => plain(wire::encode_resize_args(&wire::ResizeArgs {
            view,
            width: rng.below(9000) as u32,
            height: rng.below(9000) as u32,
        })),
        wire::METHOD_LOAD => plain(wire::encode_load_args(&wire::LoadArgs {
            view,
            url: text(rng),
        })),
        wire::METHOD_LOADHTML => {
            let (handle, _) = fx.fabric.new_buffer_with(blob(rng, 40));
            let html = Buffer {
                handle,
                offset: rng.below(4),
                len: rng.below(50),
            };
            let args = wire::LoadHtmlArgs {
                view,
                html,
                base_url: text(rng),
            };
            wire::encode_load_html_args(&args).unwrap()
        }
        wire::METHOD_RELOAD => plain(wire::encode_reload_args(&wire::ReloadArgs {
            view,
            bypass_blocking: rng.one_in(2),
        })),
        wire::METHOD_RECOVER => plain(wire::encode_recover_args(&wire::RecoverArgs {
            view,
            reload: rng.one_in(2),
        })),
        wire::METHOD_NOTIFYFORGET => {
            plain(wire::encode_notify_forget_args(&wire::NotifyForgetArgs {
                view,
                origin: text(rng),
            }))
        }
        wire::METHOD_DOWNLOAD => plain(wire::encode_download_args(&wire::DownloadArgs {
            view,
            url: text(rng),
        })),
        wire::METHOD_MOUSE => plain(wire::encode_mouse_args(&wire::MouseArgs {
            view,
            kind: rng.below(4) as u32,
            button: rng.below(5) as i32 - 1,
            x: 1,
            y: 2,
            mods: rng.next_u32(),
        })),
        wire::METHOD_KEY => plain(wire::encode_key_args(&wire::KeyArgs {
            view,
            press: rng.one_in(2),
            keysym: rng.next_u32(),
            text: text(rng),
            mods: 0,
        })),
        wire::METHOD_DIALOGREPLY => plain(wire::encode_dialog_reply_args(&wire::DialogReplyArgs {
            view,
            dialog: request,
            accepted: rng.one_in(2),
            value: text(rng),
        })),
        wire::METHOD_NAVIGATIONANSWER => plain(wire::encode_navigation_answer_args(
            &wire::NavigationAnswerArgs {
                view,
                request,
                allow: rng.one_in(2),
            },
        )),
        wire::METHOD_NOTIFYPERMISSION => plain(wire::encode_notify_permission_args(
            &wire::NotifyPermissionArgs {
                view,
                origin: text(rng),
                answer: rng.below(4) as u32,
            },
        )),
        wire::METHOD_DOWNLOADCANCEL => plain(wire::encode_download_cancel_args(
            &wire::DownloadCancelArgs {
                view,
                download: request,
            },
        )),
        wire::METHOD_SETBLOCKING => plain(wire::encode_set_blocking_args(&wire::SetBlockingArgs {
            view,
            enabled: rng.one_in(2),
            exceptions: (0..rng.below(4)).map(|_| text(rng)).collect(),
        })),
        wire::METHOD_BLOCKINGCOMPILE => {
            let (handle, _) = fx.fabric.new_buffer_with(blob(rng, 40));
            let rules = Buffer {
                handle,
                offset: rng.below(4),
                len: rng.below(50),
            };
            let args = wire::BlockingCompileArgs {
                view,
                list: text(rng),
                rules,
            };
            wire::encode_blocking_compile_args(&args).unwrap()
        }
        wire::METHOD_BLOCKINGLOAD => {
            plain(wire::encode_blocking_load_args(&wire::BlockingLoadArgs {
                view,
                list: text(rng),
            }))
        }
        wire::METHOD_BLOCKINGFORGET => plain(wire::encode_blocking_forget_args(
            &wire::BlockingForgetArgs {
                view,
                list: text(rng),
            },
        )),
        _ => (blob(rng, 30), Vec::new()),
    }
}
