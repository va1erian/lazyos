//! A seeded soak of a browser opening, closing, hiding and recovering tabs against webd's tab calls
//! (`full_tabs.rs` has the single-case tests): groups, keys, views, slot bytes and handles must stay exact.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, Fx, Via};
use support::origin;
use xui_webd::limits::VIEWS_PER_BROWSER_CLIENT;
use xui_webd::recording::Call as EngineCall;

fn tab(key: &str) -> wire::ViewConfig {
    wire::ViewConfig {
        process_key: key.into(),
        ..browser()
    }
}

fn last_group(fx: &Fx) -> i32 {
    fx.engine
        .calls()
        .iter()
        .rev()
        .find_map(|call| match call {
            EngineCall::ViewCreate(_, config) => Some(config.group),
            _ => None,
        })
        .expect("a view was created")
}

#[test]
fn a_seeded_soak_of_tabs_opening_and_closing_keeps_groups_views_and_handles_exact() {
    // Why: a browser opens and closes tabs for hours. Every key a tab used must be released with its
    // last view (the group table stays the size of the sites on screen), tabs of one site must always
    // share a group, a released group's number must never come back, and nothing may leak.
    use std::collections::{BTreeMap, BTreeSet};
    fuzzkit::for_seeds("webd_tabs_soak", |_, rng| {
        let mut fx = Fx::new("tab-soak");
        let app = origin(7);
        let baseline = fx.fabric.open_handles();
        let mut live: Vec<(u64, String, bool)> = Vec::new();
        let mut group_of: BTreeMap<String, i32> = BTreeMap::new();
        let mut used: BTreeSet<i32> = BTreeSet::new();
        for step in 0..300 {
            match rng.below(10) {
                0..=4 if live.len() < VIEWS_PER_BROWSER_CLIENT => {
                    let key = format!("site{}.test", rng.below(12));
                    let (view, _) = fx.view(app, Via::Browser, tab(&key), 8, 8);
                    let group = last_group(&fx);
                    match group_of.get(&key) {
                        Some(known) => assert_eq!(*known, group, "step {step}: {key} split"),
                        None => {
                            assert!(used.insert(group), "step {step}: group {group} reused");
                            group_of.insert(key.clone(), group);
                        }
                    }
                    live.push((view, key, false));
                }
                5 | 6 if !live.is_empty() => {
                    let at = rng.below(live.len() as u64) as usize;
                    let (view, key, _) = live.swap_remove(at);
                    fx.destroy_view(app, view).unwrap();
                    if !live.iter().any(|(_, other, _)| *other == key) {
                        group_of.remove(&key);
                    }
                }
                7 if !live.is_empty() => {
                    let at = rng.below(live.len() as u64) as usize;
                    let (view, _, attached) = &mut live[at];
                    if *attached {
                        fx.detach_slots(app, *view).unwrap();
                    } else {
                        fx.attach_frames(app, *view, 8, 8);
                    }
                    *attached = !*attached;
                }
                8 if !live.is_empty() => {
                    let at = rng.below(live.len() as u64) as usize;
                    fx.recover_group(app, live[at].0).unwrap();
                }
                _ => {}
            }
            let keys: BTreeSet<&String> = live.iter().map(|(_, key, _)| key).collect();
            assert_eq!(
                fx.service.process_keys(app.client()),
                keys.len(),
                "step {step}"
            );
            assert_eq!(
                fx.service.client_views(app.client()),
                live.len(),
                "step {step}"
            );
            let attached = live.iter().filter(|(_, _, attached)| *attached).count();
            let held = fx
                .service
                .client_usage()
                .first()
                .map_or(0, |u| u.slot_bytes);
            assert_eq!(held, attached as u64 * 3 * 8 * 8 * 4, "step {step}");
        }
        for (view, ..) in live.drain(..) {
            fx.destroy_view(app, view).unwrap();
        }
        assert_eq!(fx.service.process_keys(app.client()), 0);
        assert_eq!(fx.service.view_count(), 0);
        assert_eq!(fx.fabric.open_handles(), baseline, "a handle leaked");
        assert_eq!(fx.fabric.double_closes(), 0);
    });
}
