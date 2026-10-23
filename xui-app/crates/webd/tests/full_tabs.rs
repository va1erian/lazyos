//! What a browser needs of webd to hold tabs (docs/lazywebplus-plan.md 2.3, 8): a web process per
//! `process_key` inside the caller's own group, the larger view cap of the browser scope, the memory
//! gate and the process ceiling counting keys, `DetachSlots` for background tabs, `RecoverGroup` for a
//! hung site, and a user agent a tab may change.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, web, Fx, Via};
use support::origin;
use xui_webd::engine::{EngineEvents, LOST_OTHER};
use xui_webd::fabric::errno::*;
use xui_webd::limits::{
    PROCESS_KEYS_PER_CLIENT, VIEWS_PER_BROWSER_CLIENT, VIEWS_PER_CLIENT, WEB_PROCESSES_PER_USER,
    WEB_PROCESS_RESERVE_BYTES as RESERVE,
};
use xui_webd::recording::Call as EngineCall;

/// A browser tab's configuration for the site `key`.
fn tab(key: &str) -> wire::ViewConfig {
    wire::ViewConfig {
        process_key: key.into(),
        ..browser()
    }
}

/// The group the engine was told for the view made last.
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

fn creates(fx: &Fx) -> usize {
    fx.engine
        .calls()
        .iter()
        .filter(|call| matches!(call, EngineCall::ViewCreate(..)))
        .count()
}

#[test]
fn views_with_one_process_key_share_a_group_and_other_keys_and_clients_never_do() {
    // Why: the engine puts the views of one group on one web process, so the group the service picks is
    // the placement: tabs of a site together, sites apart, and no key reaches another client's group.
    let mut fx = Fx::new("tab-groups");
    let (app, other) = (origin(7), origin(8));
    fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    let a1 = last_group(&fx);
    fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    assert_eq!(last_group(&fx), a1, "tabs of one site share a process");
    fx.view(app, Via::Browser, tab("b.test"), 4, 4);
    let b = last_group(&fx);
    assert_ne!(b, a1, "another site has a process of its own");
    fx.view(app, Via::Browser, browser(), 4, 4);
    let default = last_group(&fx);
    assert!(default != a1 && default != b);
    assert_eq!(fx.service.group_of(app.client()), Some(default));
    fx.view(app, Via::Browser, tab(""), 4, 4);
    assert_eq!(last_group(&fx), default, "the empty key is the default");
    // The same key of another client names that client's own group.
    fx.view(other, Via::Browser, tab("a.test"), 4, 4);
    let theirs = last_group(&fx);
    assert!(![a1, b, default].contains(&theirs));
}

#[test]
fn a_process_keys_group_goes_with_its_last_view_and_its_number_is_not_reused() {
    // Why: a browser visits many sites over a session; the group table must not grow with them, and a
    // new view of a site whose process ended must not join anything old.
    let mut fx = Fx::new("tab-release");
    let app = origin(7);
    let (first, _) = fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    let (second, _) = fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    let group = last_group(&fx);
    fx.destroy_view(app, first).unwrap();
    // One tab of the site is left: the process, and so the group, lives on.
    let (third, _) = fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    assert_eq!(last_group(&fx), group);
    assert_eq!(fx.service.process_keys(app.client()), 1);
    fx.destroy_view(app, second).unwrap();
    fx.destroy_view(app, third).unwrap();
    // Every tab of the site is gone: its key is free again, with a number never used before.
    assert_eq!(fx.service.process_keys(app.client()), 0);
    fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    assert_ne!(last_group(&fx), group);
}

#[test]
fn a_client_holds_a_bounded_number_of_process_keys_even_through_lost_views() {
    // Why: a key is the caller's own split of its group. Live views are bounded by the process ceiling
    // first, but a view the engine lost holds its key until `DestroyView` and starts no process, so
    // without this bound a client could keep a group per string it names.
    let mut fx = Fx::new("tab-keys");
    let app = origin(7);
    let mut views = Vec::new();
    for n in 0..PROCESS_KEYS_PER_CLIENT {
        let (view, _) = fx.view(app, Via::Browser, tab(&format!("site{n}.test")), 4, 4);
        fx.queue.view_lost(view, LOST_OTHER);
        fx.pump(0);
        views.push(view);
    }
    assert_eq!(
        fx.service.process_keys(app.client()),
        PROCESS_KEYS_PER_CLIENT
    );
    let made = creates(&fx);
    let handles = fx.fabric.open_handles();
    assert_eq!(
        fx.create_view(app, Via::Browser, tab("one-more.test"), 4, 4),
        Err(EDQUOT)
    );
    assert_eq!(creates(&fx), made, "the engine heard of a refused view");
    assert_eq!(fx.fabric.open_handles(), handles, "the endpoint leaked");
    // A key the client holds already, the default group and another client are not affected.
    assert!(fx
        .create_view(app, Via::Browser, tab("site0.test"), 4, 4)
        .is_ok());
    assert!(fx.create_view(app, Via::Browser, browser(), 4, 4).is_ok());
    assert!(fx
        .create_view(origin(8), Via::Browser, tab("one-more.test"), 4, 4)
        .is_ok());
    // Destroying the lost view of a site frees its key for another.
    fx.destroy_view(app, views[5]).unwrap();
    assert!(fx
        .create_view(app, Via::Browser, tab("one-more.test"), 4, 4)
        .is_ok());
}

#[test]
fn the_process_ceiling_counts_keys_and_a_known_key_is_always_fine() {
    // Why: with a key per site the number of web processes is the number of sites; the user's ceiling
    // and memory charge must count them, and a tab of a site already running starts nothing.
    let mut fx = Fx::new("tab-ceiling");
    let app = origin(7);
    for n in 0..WEB_PROCESSES_PER_USER {
        fx.view(app, Via::Browser, tab(&format!("s{n}.test")), 4, 4);
    }
    let handles = fx.fabric.open_handles();
    assert_eq!(
        fx.create_view(app, Via::Browser, tab("new.test"), 4, 4),
        Err(EDQUOT)
    );
    assert_eq!(fx.fabric.open_handles(), handles, "the endpoint leaked");
    assert!(fx
        .create_view(app, Via::Browser, tab("s0.test"), 4, 4)
        .is_ok());
    // Another client of the same user shares the ceiling.
    assert_eq!(
        fx.create_view(origin(8), Via::Net, web(), 4, 4),
        Err(EDQUOT)
    );
}

#[test]
fn the_memory_gate_refuses_a_new_site_but_not_another_tab_of_a_running_one() {
    // Why: a new key starts a web process, which costs about 107 MiB; the gate that spares the user's
    // quota must see it, and a tab of a site that runs already must not be refused by it.
    let mut fx = Fx::new("tab-memory");
    let app = origin(7);
    let limit: u64 = 1 << 30;
    fx.set_memory(0, limit);
    fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    fx.set_memory(limit - RESERVE + 1, limit);
    let made = creates(&fx);
    assert_eq!(
        fx.create_view(app, Via::Browser, tab("b.test"), 4, 4),
        Err(EDQUOT)
    );
    assert_eq!(creates(&fx), made);
    assert!(fx
        .create_view(app, Via::Browser, tab("a.test"), 4, 4)
        .is_ok());
    // A refused key leaves no group behind: it is made when there is room.
    fx.set_memory(0, limit);
    assert!(fx
        .create_view(app, Via::Browser, tab("b.test"), 4, 4)
        .is_ok());
}

#[test]
fn a_browser_may_hold_sixty_four_views_and_the_other_scopes_eight() {
    // Why: a view per tab (docs/lazywebplus-plan.md 2.3); the larger cap is the browser scope's.
    assert_eq!((VIEWS_PER_CLIENT, VIEWS_PER_BROWSER_CLIENT), (8, 64));
    let mut fx = Fx::new("tab-cap");
    let (browser_app, net_app) = (origin(7), origin(8));
    for _ in 0..VIEWS_PER_BROWSER_CLIENT {
        fx.view(browser_app, Via::Browser, tab("a.test"), 4, 4);
    }
    assert_eq!(
        fx.create_view(browser_app, Via::Browser, tab("a.test"), 4, 4),
        Err(ENOSPC)
    );
    for _ in 0..VIEWS_PER_CLIENT {
        fx.view(net_app, Via::Net, web(), 4, 4);
    }
    assert_eq!(fx.create_view(net_app, Via::Net, web(), 4, 4), Err(ENOSPC));
    // The cap is the scope of the call that makes the view: a client with views through the larger
    // scope does not widen the others' calls.
    assert_eq!(
        fx.create_view(browser_app, Via::Net, web(), 4, 4),
        Err(ENOSPC)
    );
}

#[test]
fn a_bad_process_key_is_refused_before_the_engine_hears() {
    // Why: the key becomes a table key and a trace line; it must be short printable text.
    let mut fx = Fx::new("tab-key-bad");
    let app = origin(7);
    for key in [
        "k".repeat(65),
        "caf\u{e9}".into(),
        "a\nb".into(),
        "a\0b".into(),
    ] {
        let made = creates(&fx);
        let handles = fx.fabric.open_handles();
        assert_eq!(
            fx.create_view(app, Via::Browser, tab(&key), 4, 4),
            Err(EINVAL),
            "{key:?}"
        );
        assert_eq!((creates(&fx), fx.fabric.open_handles()), (made, handles));
    }
    assert!(fx
        .create_view(app, Via::Browser, tab(&"k".repeat(64)), 4, 4)
        .is_ok());
}

#[test]
fn detach_slots_gives_the_slot_memory_back_without_resizing_the_view() {
    // Why: a background tab keeps its view but not its three frame buffers, which count against the
    // client's slot budget; the page must not be told to resize (a layout).
    let mut fx = Fx::new("tab-detach");
    let (app, other) = (origin(7), origin(8));
    let (view, _) = fx.view(app, Via::Browser, tab("a.test"), 32, 32);
    fx.attach_frames(app, view, 32, 32);
    let held = |fx: &Fx| fx.service.client_usage()[0].slot_bytes;
    assert_eq!(held(&fx), 3 * 32 * 32 * 4);
    fx.engine.take();
    assert_eq!(fx.detach_slots(other, view), Err(ENOENT));
    assert_eq!(held(&fx), 3 * 32 * 32 * 4, "another client detached");
    // The reply names the last frame sent, so the app can tell the ones still queued for it.
    let sent = |fx: &mut Fx| {
        let body = fx.detach_slots(app, view).unwrap();
        wire::decode_detach_slots_reply(&body).unwrap().seq
    };
    assert_eq!(sent(&mut fx), 0, "no frame was sent yet");
    fx.attach_frames(app, view, 32, 32);
    fx.rig.paint(view, 32, 32, 7);
    fx.pump(0);
    assert_eq!(sent(&mut fx), 1);
    fx.attach_frames(app, view, 32, 32);
    fx.rig.paint(view, 32, 32, 8);
    fx.pump(1);
    assert_eq!(sent(&mut fx), 2, "the count goes on across detaches");
    fx.attach_frames(app, view, 32, 32);
    fx.detach_slots(app, view).unwrap();
    assert_eq!(held(&fx), 0);
    assert!(
        !fx.engine
            .take()
            .iter()
            .any(|call| matches!(call, EngineCall::Resize(..))),
        "a detach is not a resize"
    );
    // The view works: slots attach again, and again, with a whole frame asked for.
    fx.attach_frames(app, view, 32, 32);
    assert_eq!(held(&fx), 3 * 32 * 32 * 4);
    fx.detach_slots(app, view).unwrap();
    fx.detach_slots(app, view).unwrap();
    // A light view is not a full view.
    let lite = fx.rig.create(app, Default::default(), 8, 8).unwrap().0;
    assert_eq!(fx.detach_slots(app, lite), Err(ENOENT));
    assert_eq!(fx.detach_slots(app, view + 1000), Err(ENOENT));
    assert!(fx.call(app, wire::METHOD_DETACHSLOTS, vec![1, 2]).is_err());
}

#[test]
fn recover_group_reaches_the_engine_for_the_callers_live_view_only() {
    // Why: it ends a web process that other tabs share, so only the owner may ask, a lost view has
    // nothing to recover, and a refused call must not reach the engine.
    let mut fx = Fx::new("tab-recover-group");
    let (app, other) = (origin(7), origin(8));
    let (view, _) = fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    fx.view(app, Via::Browser, tab("a.test"), 4, 4);
    fx.engine.take();
    fx.recover_group(app, view).unwrap();
    assert_eq!(fx.engine.take(), vec![EngineCall::RecoverGroup(view)]);
    assert_eq!(fx.recover_group(other, view), Err(ENOENT));
    assert_eq!(fx.recover_group(app, view + 1000), Err(ENOENT));
    assert!(fx.call(app, wire::METHOD_RECOVERGROUP, vec![9]).is_err());
    assert!(
        fx.engine.take().is_empty(),
        "a refused call reached the engine"
    );
    fx.queue.view_lost(view, LOST_OTHER);
    fx.pump(0);
    assert_eq!(fx.recover_group(app, view), Err(ENODEV));
    assert!(fx.engine.take().is_empty());
}

#[test]
fn a_process_key_needs_the_browser_profile_on_disk() {
    // Why: the engine shares an in-memory store only through a live group mate, so keyed views of a private
    // window (or of an app's own profile) would each get a store of their own and share no cookies. The
    // refusal also keeps keys, and the extra processes they start, to the browser scope.
    let mut fx = Fx::new("tab-key-memory");
    let app = origin(7);
    let private = wire::ViewConfig {
        ephemeral: true,
        ..tab("a.test")
    };
    assert_eq!(
        fx.create_view(app, Via::Browser, private, 4, 4),
        Err(EINVAL)
    );
    // An empty key is the group of all of the client's private windows, as before.
    let shared = wire::ViewConfig {
        ephemeral: true,
        ..browser()
    };
    assert!(fx.create_view(app, Via::Browser, shared, 4, 4).is_ok());
    for via in [Via::View, Via::Net] {
        let config = wire::ViewConfig {
            process_key: "a.test".into(),
            ..if via == Via::View {
                support::full::data_only()
            } else {
                web()
            }
        };
        assert_eq!(
            fx.create_view(app, via, config, 4, 4),
            Err(EINVAL),
            "{via:?}"
        );
    }
    assert_eq!(
        fx.service.process_keys(app.client()),
        0,
        "a refusal kept a key"
    );
}

#[test]
fn downloads_are_limited_per_client_not_per_process_key() {
    // Why: the shared saver caps the downloads of one owner so a client cannot use up the others' places.
    // The owner is the client, so tabs of many sites together stay under one cap.
    let mut fx = Fx::new("tab-downloads");
    let (app, other) = (origin(7), origin(8));
    let downloading = |key: &str| wire::ViewConfig {
        downloads: true,
        ..tab(key)
    };
    let per_client = lazydownloads::MAX_DOWNLOADS_PER_OWNER as u64;
    let views: Vec<u64> = (0..4)
        .map(|n| {
            fx.view(
                app,
                Via::Browser,
                downloading(&format!("site{n}.test")),
                4,
                4,
            )
            .0
        })
        .collect();
    let (neighbour, _) = fx.view(other, Via::Browser, downloading("a.test"), 4, 4);
    let start = |fx: &Fx, view, id: u64| {
        fx.queue
            .download_start(view, id, "http://x/", &format!("f{id}"), 1)
    };
    for id in 1..=per_client {
        let view = views[(id % 4) as usize];
        assert!(start(&fx, view, id).is_some(), "{id}");
    }
    for view in &views {
        assert_eq!(
            start(&fx, *view, 9999),
            None,
            "a tab of another site got places"
        );
    }
    assert!(start(&fx, neighbour, 1).is_some());
}
