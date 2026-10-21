//! Seeded fuzz (`FUZZ_CASES`, `FUZZ_SEED` as in `fuzzkit`): the slot state machine against a model,
//! the whole service against random traffic from two clients and an engine, and the request
//! decoders against random and mutated bodies. A failing seed is printed with its replay command.

#![cfg(not(feature = "host"))]

mod support;

use fuzzkit::{for_seeds, Rng};
use libmessenger::{Buffer, Object};
use messenger_generated::os_lazy_webd_light_v1 as wire;
use support::{origin, rig, Ev, Rig};
use xui_webd::engine::EngineEvents;
use xui_webd::ident::Origin;
use xui_webd::limits::{SLOTS_PER_VIEW, VIEWS_PER_CLIENT};
use xui_webd::slots::{AttachError, Slots};

#[test]
fn slots_match_a_model() {
    for_seeds("webd_slots_model", |_, rng| {
        let mut slots: Slots<u32> = Slots::default();
        // The model: per slot, None or (memory, bytes, busy).
        let mut model: [Option<(u32, u64, bool)>; SLOTS_PER_VIEW] = [None; SLOTS_PER_VIEW];
        for step in 0..400u32 {
            match rng.below(6) {
                0 | 1 => {
                    let index = rng.below(SLOTS_PER_VIEW as u64 + 1) as usize;
                    let bytes = rng.below(1000);
                    let got = slots.attach(index, step, bytes);
                    let want = match model.get(index) {
                        None => Err(AttachError::OutOfRange),
                        Some(Some((_, _, true))) => Err(AttachError::Busy),
                        Some(_) => {
                            model[index] = Some((step, bytes, false));
                            Ok(())
                        }
                    };
                    assert_eq!(got, want, "attach {index}");
                }
                2 | 3 => {
                    let got = slots.acquire();
                    let want = model
                        .iter()
                        .position(|slot| matches!(slot, Some((_, _, false))));
                    if let Some(index) = want {
                        model[index].as_mut().unwrap().2 = true;
                    }
                    assert_eq!(got, want, "acquire");
                }
                4 => {
                    let index = rng.below(SLOTS_PER_VIEW as u64 + 1) as usize;
                    let want = matches!(model.get(index), Some(Some((_, _, true))));
                    if want {
                        model[index].as_mut().unwrap().2 = false;
                    }
                    assert_eq!(slots.release(index), want, "release {index}");
                }
                _ => {
                    if rng.one_in(4) {
                        slots.detach_all();
                        model = [None; SLOTS_PER_VIEW];
                    }
                }
            }
            let attached = model.iter().flatten().count();
            let busy = model.iter().flatten().filter(|slot| slot.2).count();
            assert_eq!(slots.counts(), (attached, busy));
            assert_eq!(
                slots.bytes(),
                model.iter().flatten().map(|slot| slot.1).sum::<u64>()
            );
            assert_eq!(slots.has_free(), attached > busy);
            for (index, slot) in model.iter().enumerate() {
                assert_eq!(slots.is_busy(index), matches!(slot, Some((_, _, true))));
                assert_eq!(slots.memory_mut(index).copied(), slot.map(|slot| slot.0));
            }
        }
    });
}

/// What the test knows about one view it made.
struct Known {
    client: usize,
    view: u64,
    channel: u64,
    width: u32,
    height: u32,
    busy: [bool; SLOTS_PER_VIEW],
    last_seq: u64,
    lost: bool,
    ended: bool,
}

fn clients() -> [Origin; 2] {
    [origin(7), origin(8)]
}

#[test]
fn random_traffic_keeps_every_rule() {
    for_seeds("webd_traffic", |_, rng| traffic(rng));
}

fn traffic(rng: &mut Rng) {
    let mut rig = rig();
    let who = clients();
    let mut known: Vec<Known> = Vec::new();
    let mut now = 0u64;
    for _ in 0..300 {
        let client = rng.below(2) as usize;
        match rng.below(14) {
            0 | 1 => {
                let (w, h) = (rng.range(1, 6) as u32, rng.range(1, 6) as u32);
                // A client that was found dead has lost views the test has not noticed, so the
                // service's own count is the one to hold to the quota.
                let before = rig.service.client_views(who[client].client());
                match rig.create(who[client], wire::LightConfig::default(), w, h) {
                    Ok((view, channel)) => {
                        assert!(before < VIEWS_PER_CLIENT, "created past the quota");
                        assert!(known.iter().all(|k| k.view != view), "an id was reused");
                        known.push(Known {
                            client,
                            view,
                            channel,
                            width: w,
                            height: h,
                            busy: [false; SLOTS_PER_VIEW],
                            last_seq: 0,
                            lost: false,
                            ended: false,
                        });
                    }
                    Err(_) => assert!(
                        before >= VIEWS_PER_CLIENT || (w, h) == (0, 0),
                        "refused below the quota"
                    ),
                }
            }
            2 => {
                if let Some(k) = pick(rng, &mut known, None) {
                    // Sometimes as the wrong client: it must change nothing.
                    let caller = if rng.one_in(4) {
                        1 - k.client
                    } else {
                        k.client
                    };
                    rig.destroy(who[caller], k.view).unwrap();
                    if caller == k.client {
                        k.ended = true;
                    }
                }
            }
            3 | 4 => {
                if let Some(k) = pick(rng, &mut known, None) {
                    let slot = rng.below(SLOTS_PER_VIEW as u64 + 1) as u32;
                    let right_size = (k.width * k.height * 4) as usize;
                    let bytes = if rng.one_in(5) {
                        right_size.saturating_sub(1)
                    } else {
                        right_size + rng.below(8) as usize
                    };
                    let caller = if rng.one_in(6) {
                        1 - k.client
                    } else {
                        k.client
                    };
                    let (outcome, _) = rig.attach_bytes(who[caller], k.view, slot, bytes);
                    if outcome.is_ok() {
                        assert_eq!(caller, k.client, "someone else's view took a slot");
                        assert!(bytes >= right_size && (slot as usize) < SLOTS_PER_VIEW);
                    }
                }
            }
            5 | 6 => {
                if let Some(k) = pick(rng, &mut known, None) {
                    let slot = rng.below(SLOTS_PER_VIEW as u64) as u32;
                    if rig.release(who[k.client], k.view, slot).is_ok() {
                        k.busy[slot as usize] = false;
                    }
                }
            }
            7 => {
                if let Some(k) = pick(rng, &mut known, None) {
                    let (w, h) = (rng.range(1, 6) as u32, rng.range(1, 6) as u32);
                    if rig.resize(who[k.client], k.view, w, h).is_ok() {
                        (k.width, k.height, k.busy) = (w, h, [false; SLOTS_PER_VIEW]);
                    }
                }
            }
            8..=10 => {
                // The engine paints (sometimes the wrong size) or reports.
                if let Some(k) = pick(rng, &mut known, None) {
                    let (w, h) = if rng.one_in(6) {
                        (k.width + 1, k.height)
                    } else {
                        (k.width, k.height)
                    };
                    match rng.below(5) {
                        0..=2 => rig.paint(k.view, w as i32, h as i32, rng.byte()),
                        3 => rig
                            .queue
                            .state(k.view, "t", rng.one_in(2), rng.below(4) as i32, ""),
                        _ => rig.queue.link_clicked(k.view, "https://example.com/"),
                    }
                }
            }
            11 => {
                if let Some(k) = pick(rng, &mut known, None) {
                    match rng.below(4) {
                        0 => {
                            rig.queue.view_lost(k.view, rng.below(6) as i32);
                        }
                        1 => rig.fabric.set_full(k.channel, rng.one_in(2)),
                        2 => rig.fabric.kill(k.channel),
                        _ => {
                            let html = rig.load_html(who[k.client], k.view, b"<p>x</p>", "");
                            assert!(html.is_err() || !(k.lost || k.ended));
                        }
                    }
                }
            }
            _ => {
                now += rng.below(1500);
                rig.service.pump(now);
                observe(&mut rig, &mut known);
            }
        }
        assert_eq!(rig.fabric.double_closes(), 0);
    }
    now += 2000;
    rig.service.pump(now);
    observe(&mut rig, &mut known);
    rig.service.shutdown();
    assert_eq!(rig.service.view_count(), 0);
    assert_eq!(rig.fabric.open_handles(), 0, "a handle leaked");
    assert_eq!(rig.fabric.double_closes(), 0);
}

/// Up to `max` random bytes.
fn blob(rng: &mut Rng, max: u64) -> Vec<u8> {
    let len = rng.below(max) as usize;
    rng.bytes(len)
}

fn pick<'a>(rng: &mut Rng, known: &'a mut [Known], _hint: Option<u64>) -> Option<&'a mut Known> {
    if known.is_empty() {
        return None;
    }
    let index = rng.below(known.len() as u64) as usize;
    Some(&mut known[index])
}

/// Read every channel's events and check them against what the client knows.
fn observe(rig: &mut Rig, known: &mut [Known]) {
    for k in known.iter_mut() {
        for ev in rig.events(k.channel) {
            match ev {
                Ev::Frame(frame) => {
                    assert_eq!(frame.view, k.view, "an event on the wrong channel");
                    let slot = frame.slot as usize;
                    assert!(
                        slot < SLOTS_PER_VIEW && !k.busy[slot],
                        "a frame in a busy slot"
                    );
                    assert_eq!(frame.seq, k.last_seq + 1, "frames are numbered one by one");
                    assert_eq!((frame.width, frame.height), (k.width, k.height));
                    assert!(!k.lost && !k.ended);
                    (k.busy[slot], k.last_seq) = (true, frame.seq);
                }
                Ev::State(e) => assert_eq!(e.view, k.view),
                Ev::Scroll(e) => assert_eq!(e.view, k.view),
                Ev::Link(e) => assert_eq!(e.view, k.view),
                Ev::Clipboard(e) => assert_eq!(e.view, k.view),
                Ev::Lost(e) => {
                    assert_eq!(e.view, k.view);
                    k.lost = true;
                }
                Ev::Ping => {}
            }
        }
    }
}

#[test]
fn random_and_mutated_requests_never_panic_and_never_leak_a_handle() {
    for_seeds("webd_decode", |_, rng| {
        let mut rig = rig();
        let app = origin(7);
        // One real view so the valid-looking requests get past the lookup.
        let (view, _) = rig.create_plain(app, 4, 4);
        let methods = [
            wire::METHOD_CREATEVIEW,
            wire::METHOD_DESTROYVIEW,
            wire::METHOD_ATTACHSLOT,
            wire::METHOD_RELEASESLOT,
            wire::METHOD_RESIZE,
            wire::METHOD_SETVISIBLE,
            wire::METHOD_REFRESH,
            wire::METHOD_LOADHTML,
            wire::METHOD_MOUSE,
            wire::METHOD_WHEEL,
            wire::METHOD_KEY,
            wire::METHOD_FOCUS,
            0,
            99,
            4242,
        ];
        for _ in 0..200 {
            let method = *rng.pick(&methods);
            let (mut body, mut objects) = valid_request(rng, &mut rig, method, view);
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
                // Objects the request does not carry were never installed: close them as the
                // kernel would not have made them.
                0 => objects.drain(..).for_each(|object| close(&rig, object)),
                1 => objects.push(Object::Buffer(rig.fabric.new_buffer(8).0)),
                2 => objects.push(Object::Channel(rig.fabric.new_channel())),
                _ => {}
            }
            let _ = rig.raw(*rng.pick(&clients()), method, body, objects);
            assert_eq!(rig.fabric.double_closes(), 0);
        }
        rig.service.shutdown();
        assert_eq!(rig.fabric.open_handles(), 0, "a handle leaked");
    });
}

fn close(rig: &Rig, object: Object) {
    use xui_webd::fabric::Fabric;
    match object {
        Object::Channel(handle) => rig.fabric.close_channel(handle),
        Object::Buffer(handle) => rig.fabric.close_buffer(handle),
    }
}

/// A well-formed request for `method` with fresh objects.
fn valid_request(rng: &mut Rng, rig: &mut Rig, method: u32, view: u64) -> (Vec<u8>, Vec<Object>) {
    let slot = rng.below(4) as u32;
    let plain = |body: Result<Vec<u8>, libmessenger::Error>| (body.unwrap(), Vec::new());
    match method {
        wire::METHOD_CREATEVIEW => {
            let events = rig.fabric.new_channel();
            let config = wire::LightConfig {
                scripts: rng.one_in(2),
                color_scheme: rng.below(3) as u32,
                default_font_size: rng.below(40) as u32,
                user_style: String::new(),
            };
            wire::encode_create_view_args(&wire::CreateViewArgs {
                config,
                width: rng.below(9000) as u32,
                height: rng.below(9000) as u32,
                events,
            })
            .unwrap()
        }
        wire::METHOD_DESTROYVIEW => plain(wire::encode_destroy_view_args(&wire::DestroyViewArgs {
            view,
        })),
        wire::METHOD_ATTACHSLOT => {
            let (handle, _) = rig.fabric.new_buffer(rng.below(200) as usize);
            let pixels = Buffer {
                handle,
                offset: rng.below(8),
                len: rng.below(200),
            };
            wire::encode_attach_slot_args(&wire::AttachSlotArgs { view, slot, pixels }).unwrap()
        }
        wire::METHOD_RELEASESLOT => plain(wire::encode_release_slot_args(&wire::ReleaseSlotArgs {
            view,
            slot,
        })),
        wire::METHOD_RESIZE => plain(wire::encode_resize_args(&wire::ResizeArgs {
            view,
            width: rng.below(9000) as u32,
            height: rng.below(9000) as u32,
        })),
        wire::METHOD_SETVISIBLE => plain(wire::encode_set_visible_args(&wire::SetVisibleArgs {
            view,
            visible: rng.one_in(2),
        })),
        wire::METHOD_REFRESH => plain(wire::encode_refresh_args(&wire::RefreshArgs { view })),
        wire::METHOD_LOADHTML => {
            let (handle, _) = rig.fabric.new_buffer_with(blob(rng, 40));
            let html = Buffer {
                handle,
                offset: rng.below(4),
                len: rng.below(50),
            };
            let base_url = (*rng.pick(&["", "about:blank", "http://x/", "data:,"])).to_owned();
            wire::encode_load_html_args(&wire::LoadHtmlArgs {
                view,
                html,
                base_url,
            })
            .unwrap()
        }
        wire::METHOD_MOUSE => plain(wire::encode_mouse_args(&wire::MouseArgs {
            view,
            kind: rng.below(4) as u32,
            button: rng.below(5) as i32 - 1,
            x: 1,
            y: 2,
            mods: rng.next_u32(),
        })),
        wire::METHOD_WHEEL => plain(wire::encode_wheel_args(&wire::WheelArgs {
            view,
            x: 1,
            y: 2,
            ticks_x: 3,
            ticks_y: -4,
            mods: rng.next_u32(),
        })),
        wire::METHOD_KEY => plain(wire::encode_key_args(&wire::KeyArgs {
            view,
            press: rng.one_in(2),
            keysym: rng.next_u32(),
            text: "a".into(),
            mods: 0,
        })),
        wire::METHOD_FOCUS => plain(wire::encode_focus_args(&wire::FocusArgs {
            view,
            focused: rng.one_in(2),
        })),
        _ => (blob(rng, 30), Vec::new()),
    }
}
