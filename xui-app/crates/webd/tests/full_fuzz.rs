//! Seeded fuzz (`FUZZ_CASES`, `FUZZ_SEED` as in `fuzzkit`) of the whole service with full and light
//! views: two clients make views through every scope, attach and release slots, call everything
//! the full interface has (some as the wrong client), while the engine paints, scrolls, sends
//! deltas and bands, asks questions, downloads and notifies. After every pump the events each
//! client got are checked against what it knows (`support/full_model.rs`). A failing seed is
//! printed with its replay command.

#![cfg(not(feature = "host"))]

mod support;

use std::sync::atomic::Ordering;

use fuzzkit::{for_seeds, Rng};
use messenger_generated::os_lazy_webd_light_v1 as light;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, data_only, listed, web, Fx, Via};
use support::full_model::{client_call, engine_news, light_news, observe, Known, SEEN, SEEN_NAMES};
use support::origin;
use xui_webd::engine::EngineEvents;
use xui_webd::fabric::errno::*;
use xui_webd::ident::Origin;
use xui_webd::limits::{SLOTS_PER_VIEW, SLOTS_WITH_BANDS};
use xui_webd::outside::ToastNews;

fn rank(via: Via) -> u8 {
    match via {
        Via::View => 0,
        Via::Net => 1,
        Via::Browser => 2,
    }
}

/// The least scope a config needs (the table in `fullconfig.rs`).
fn needs(config: &wire::ViewConfig) -> Via {
    if config.profile == "browser"
        || config.handoff == wire::HANDOFF_BROWSER
        || config.downloads
        || config.blocking
    {
        Via::Browser
    } else if config.scheme_policy != wire::SCHEME_POLICY_DATA_ONLY {
        Via::Net
    } else {
        Via::View
    }
}

fn random_config(rng: &mut Rng) -> wire::ViewConfig {
    let mut config = match rng.below(5) {
        0 => data_only(),
        1 => web(),
        2 => listed(&["http://10.0.2.2:18081", "https://example.com"]),
        _ => browser(),
    };
    config.deltas = rng.one_in(2);
    config.dialogs = rng.one_in(2);
    config.popups = rng.one_in(2);
    config.handoff = rng.below(2) as u32;
    if config.profile == "browser" {
        config.downloads = rng.one_in(2);
        config.blocking = rng.one_in(2) && config.scheme_policy != wire::SCHEME_POLICY_DATA_ONLY;
        config.notifications = rng.one_in(2);
        config.ephemeral = rng.one_in(4);
        config.handoff = rng.below(3) as u32;
    } else if rng.one_in(8) {
        // Sometimes something the scope or the rules refuse.
        config.profile = ["browser", "x"][rng.below(2) as usize].into();
        config.scheme_policy = rng.below(4) as u32;
        config.handoff = rng.below(4) as u32;
    }
    config
}

/// A view that was made satisfies the rules of its scope and config.
fn check_created(via: Via, config: &wire::ViewConfig) {
    assert!(rank(via) >= rank(needs(config)), "{via:?} made {config:?}");
    assert!(config.scheme_policy <= wire::SCHEME_POLICY_ORIGINS);
    assert!(config.handoff <= wire::HANDOFF_BROWSER);
    assert!(!config.notifications || (config.profile == "browser" && !config.ephemeral));
    assert!(!config.blocking || config.scheme_policy != wire::SCHEME_POLICY_DATA_ONLY);
}

fn who() -> [Origin; 2] {
    [origin(7), origin(8)]
}

#[test]
fn random_traffic_keeps_every_rule_of_the_full_interface() {
    for_seeds("webd_full_traffic", |_, rng| traffic(rng));
    // A replay of one seed need not reach everything.
    if std::env::var("FUZZ_SEED").is_err() {
        for (kind, name) in SEEN_NAMES.iter().enumerate() {
            let seen = SEEN[kind].load(Ordering::Relaxed);
            assert!(seen > 0, "the fuzz never saw a {name}");
        }
    }
}

fn traffic(rng: &mut Rng) {
    let mut fx = Fx::new("fuzz-traffic");
    let who = who();
    let mut known: Vec<Known> = Vec::new();
    let (mut now, mut ids) = (0u64, 1u64);
    for _ in 0..400 {
        match rng.below(24) {
            0 | 1 => create(&mut fx, rng, &who, &mut known),
            2 if rng.one_in(3) => {
                if let Some(k) = pick(rng, &mut known) {
                    let caller = if rng.one_in(4) {
                        1 - k.client
                    } else {
                        k.client
                    };
                    let result = if k.full {
                        fx.destroy_view(who[caller], k.view)
                    } else {
                        fx.rig.destroy(who[caller], k.view)
                    };
                    assert!(result.is_ok(), "destroy never reveals anything");
                }
            }
            3 | 4 => {
                if let Some(k) = pick(rng, &mut known) {
                    attach(&mut fx, rng, &who, k);
                }
            }
            // The client sets up its slots properly, and reads and releases what it was sent: the
            // random attaches and releases alone rarely get a picture through.
            5..=7 => {
                if let Some(k) = pick(rng, &mut known) {
                    equip(&mut fx, &who, k);
                }
            }
            8 | 9 => {
                if let Some(k) = pick(rng, &mut known) {
                    consume(&mut fx, &who, k);
                }
            }
            10 => {
                if let Some(k) = pick(rng, &mut known) {
                    let slot = rng.below(SLOTS_WITH_BANDS as u64 + 1) as u32;
                    let result = if k.full {
                        fx.release_slot(who[k.client], k.view, slot)
                    } else {
                        fx.rig.release(who[k.client], k.view, slot)
                    };
                    if result.is_ok() && (slot as usize) < SLOTS_WITH_BANDS {
                        k.busy[slot as usize] = false;
                    }
                }
            }
            11 if rng.one_in(3) => {
                if let Some(k) = pick(rng, &mut known) {
                    let (w, h) = (rng.range(1, 6) as u32, rng.range(1, 6) as u32);
                    let result = if k.full {
                        fx.resize_view(who[k.client], k.view, w, h)
                    } else {
                        fx.rig.resize(who[k.client], k.view, w, h)
                    };
                    if result.is_ok() {
                        (k.width, k.height) = (w, h);
                        k.detach_all();
                    }
                }
            }
            12..=16 => {
                if let Some(k) = pick(rng, &mut known) {
                    let (view, full, size) = (k.view, k.full, (k.width as i32, k.height as i32));
                    if full {
                        ids += 1;
                        engine_news(&mut fx, rng, view, size, ids);
                    } else {
                        light_news(&mut fx, rng, view, size);
                    }
                }
            }
            17 | 18 => {
                if let Some(k) = pick(rng, &mut known) {
                    let (view, client) = (k.view, k.client);
                    client_call(&mut fx, rng, &who, client, view, ids);
                }
            }
            19 => {
                if let Some(k) = pick(rng, &mut known) {
                    match rng.below(4) {
                        0 if rng.one_in(3) => fx.queue.view_lost(k.view, rng.below(6) as i32),
                        1 => fx.fabric.set_full(k.channel, rng.one_in(2)),
                        2 if rng.one_in(3) => fx.fabric.kill(k.channel),
                        _ => {
                            fx.outside.set_open_result(rng.one_in(2));
                            let toast = 100 + rng.below(8);
                            fx.outside.news(ToastNews::Clicked { toast });
                        }
                    }
                }
            }
            _ => {
                now += rng.below(3000);
                fx.set_clock(now + 1);
                fx.pump(now);
                observe(&mut fx, &mut known);
            }
        }
        assert_eq!(fx.fabric.double_closes(), 0);
    }
    now += 20_000;
    fx.pump(now);
    observe(&mut fx, &mut known);
    fx.service.shutdown();
    assert_eq!(fx.service.view_count(), 0);
    assert_eq!(fx.fabric.open_handles(), 0, "a handle leaked");
    assert_eq!(fx.fabric.double_closes(), 0);
}

/// The owner attaches every slot its view can use, of the right size.
fn equip(fx: &mut Fx, who: &[Origin; 2], k: &mut Known) {
    let frame = (k.width * k.height * 4) as usize;
    let band = (k.width * 1024 * 4) as usize;
    let count = if k.full && k.deltas {
        SLOTS_WITH_BANDS
    } else {
        SLOTS_PER_VIEW
    };
    let missing: Vec<usize> = (0..count).filter(|slot| !k.attached[*slot]).collect();
    for slot in missing {
        let bytes = if slot >= SLOTS_PER_VIEW { band } else { frame };
        let (result, _) = if k.full {
            fx.attach_slot(who[k.client], k.view, slot as u32, bytes)
        } else {
            fx.rig
                .attach_bytes(who[k.client], k.view, slot as u32, bytes)
        };
        k.attached[slot] |= result.is_ok();
    }
}

/// The owner is done with every picture it was sent.
fn consume(fx: &mut Fx, who: &[Origin; 2], k: &mut Known) {
    let busy: Vec<usize> = (0..SLOTS_WITH_BANDS).filter(|slot| k.busy[*slot]).collect();
    for slot in busy {
        let result = if k.full {
            fx.release_slot(who[k.client], k.view, slot as u32)
        } else {
            fx.rig.release(who[k.client], k.view, slot as u32)
        };
        k.busy[slot] &= result.is_err();
    }
}

fn pick<'a>(rng: &mut Rng, known: &'a mut [Known]) -> Option<&'a mut Known> {
    if known.is_empty() {
        return None;
    }
    let index = rng.below(known.len() as u64) as usize;
    Some(&mut known[index])
}

fn create(fx: &mut Fx, rng: &mut Rng, who: &[Origin; 2], known: &mut Vec<Known>) {
    let client = rng.below(2) as usize;
    let (w, h) = (rng.range(1, 6) as u32, rng.range(1, 6) as u32);
    if rng.one_in(4) {
        let config = light::LightConfig::default();
        if let Ok((view, channel)) = fx.rig.create(who[client], config, w, h) {
            known.push(Known::new(client, view, channel, (w, h), false, false));
        }
        return;
    }
    let via = [Via::View, Via::Net, Via::Browser][rng.below(3) as usize];
    let config = random_config(rng);
    match fx.create_view(who[client], via, config.clone(), w, h) {
        Ok((view, channel)) => {
            check_created(via, &config);
            assert!(known.iter().all(|k| k.view != view), "an id was reused");
            let deltas = config.deltas;
            known.push(Known::new(client, view, channel, (w, h), true, deltas));
        }
        Err(code) => assert!(
            [EACCES, EINVAL, ENOSPC, EDQUOT].contains(&code),
            "{via:?} {config:?}: errno {code}"
        ),
    }
}

fn attach(fx: &mut Fx, rng: &mut Rng, who: &[Origin; 2], k: &mut Known) {
    let slot = rng.below(SLOTS_WITH_BANDS as u64 + 1) as u32;
    let frame = (k.width * k.height * 4) as usize;
    let band = (k.width * 1024 * 4) as usize;
    let bytes = [frame, frame - 1, frame + 7, band, band - 1][rng.below(5) as usize];
    let caller = if rng.one_in(6) {
        1 - k.client
    } else {
        k.client
    };
    let (result, _) = if k.full {
        fx.attach_slot(who[caller], k.view, slot, bytes)
    } else {
        fx.rig.attach_bytes(who[caller], k.view, slot, bytes)
    };
    if result.is_ok() {
        assert_eq!(caller, k.client, "someone else's view took a slot");
        let index = slot as usize;
        let band_slot = index >= SLOTS_PER_VIEW;
        assert!(index < SLOTS_PER_VIEW || (k.full && k.deltas && index < SLOTS_WITH_BANDS));
        let needed = if band_slot { band } else { frame };
        assert!(bytes >= needed, "a slot too small");
        assert!(!k.busy[index], "a busy slot was replaced");
        k.attached[index] = true;
    }
}
