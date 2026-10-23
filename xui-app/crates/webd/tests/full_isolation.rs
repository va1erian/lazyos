//! The walls around a full view: light and full views are different kinds, one client's view is
//! another's "no such view", events carry the interface the view was made through, and the web
//! process cap and view count are enforced for the full interface.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_light_v1 as light;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, data_only, web, Fx, Via};
use support::origin;
use xui_webd::engine::{EngineEvents, LOST_CRASHED};
use xui_webd::events::PageNow;
use xui_webd::fabric::errno::*;
use xui_webd::ident::Origin;
use xui_webd::limits::WEB_PROCESSES_PER_USER;

#[test]
fn light_calls_on_a_full_view_and_full_calls_on_a_light_view_find_no_such_view() {
    // Why: the two kinds of view have different rules (slots, base URLs, input stamping); a call on
    // the wrong interface must not apply one kind's rules to the other, and must not tell which
    // kind it was.
    let mut fx = Fx::new("kinds");
    let app = origin(7);
    let (full, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let (lite, _) = fx.rig.create_plain(app, 4, 4);
    fx.engine.take();
    let before = fx.fabric.open_handles();

    assert_eq!(fx.rig.resize(app, full, 8, 8), Err(ENOENT));
    assert_eq!(fx.rig.load_html(app, full, b"<p>", ""), Err(ENOENT));
    assert_eq!(fx.rig.attach_bytes(app, full, 0, 64).0, Err(ENOENT));
    assert_eq!(fx.rig.release(app, full, 0), Err(ENOENT));

    assert_eq!(fx.resize_view(app, lite, 8, 8), Err(ENOENT));
    assert_eq!(fx.load_page(app, lite, b"<p>", ""), Err(ENOENT));
    assert_eq!(fx.attach_slot(app, lite, 0, 64).0, Err(ENOENT));
    assert_eq!(fx.release_slot(app, lite, 0), Err(ENOENT));
    assert_eq!(fx.load_url(app, lite, "about:blank"), Err(ENOENT));
    assert_eq!(fx.reload(app, lite, false), Err(ENOENT));
    assert_eq!(fx.stop(app, lite), Err(ENOENT));
    assert_eq!(fx.recover(app, lite, true), Err(ENOENT));
    assert_eq!(fx.notify_forget(app, lite, "https://a.test"), Err(ENOENT));
    assert_eq!(fx.notify_state(app, lite, 1, 0), Err(ENOENT));
    assert_eq!(fx.back(app, lite), Err(ENOENT));
    assert_eq!(fx.forward(app, lite), Err(ENOENT));
    assert_eq!(fx.mouse(app, lite, 1), Err(ENOENT));
    assert_eq!(fx.key(app, lite, true), Err(ENOENT));
    assert_eq!(fx.dialog_reply(app, lite, 1, true, ""), Err(ENOENT));
    assert_eq!(fx.nav_answer(app, lite, 1, true), Err(ENOENT));
    assert_eq!(fx.set_blocking(app, lite, true, &[]), Err(ENOENT));
    assert!(
        fx.engine.take().is_empty(),
        "the engine heard of a refused call"
    );
    // Only the two attach/load buffers were passed; both were closed.
    assert_eq!(fx.fabric.open_handles(), before);

    // Destroy succeeds for a view it does not own (it answers as for one that is gone) but ends
    // nothing: the right interface does.
    fx.destroy_view(app, lite).unwrap();
    fx.rig.destroy(app, full).unwrap();
    assert_eq!(fx.service.view_count(), 2);
    fx.destroy_view(app, full).unwrap();
    fx.rig.destroy(app, lite).unwrap();
    assert_eq!(fx.service.view_count(), 0);
    let destroyed = |view| fx.engine.creates_and_destroys(view).1;
    assert_eq!((destroyed(full), destroyed(lite)), (1, 1));
    assert_eq!(fx.fabric.open_handles(), 0);
}

#[test]
fn a_full_view_of_another_client_is_unusable_and_looks_like_one_that_does_not_exist() {
    // Why: view ids are small numbers any client can guess; ownership is the kernel-stamped
    // (user, package) of the sender, and a refusal must not reveal that the view exists.
    let mut fx = Fx::new("other-client");
    let (alice, bob) = (origin(7), origin(8));
    let everything = wire::ViewConfig {
        downloads: true,
        blocking: true,
        notifications: true,
        dialogs: true,
        ..browser()
    };
    let (view, _) = fx.view(alice, Via::Browser, everything, 4, 4);
    fx.engine.take();
    let before = fx.fabric.open_handles();
    let allow = wire::PERMISSION_ANSWER_ALLOW;
    let calls = [
        fx.load_url(bob, view, "about:blank"),
        fx.reload(bob, view, true),
        fx.stop(bob, view),
        fx.recover(bob, view, true),
        fx.recover(bob, view, false),
        fx.recover_group(bob, view),
        fx.detach_slots(bob, view),
        fx.set_user_agent(bob, view, "Agent/1"),
        fx.notify_forget(bob, view, "https://example.com"),
        fx.notify_state(bob, view, 1, 1),
        fx.back(bob, view),
        fx.forward(bob, view),
        fx.download(bob, view, "http://example.com/f"),
        fx.download_cancel(bob, view, 1),
        fx.dialog_reply(bob, view, 1, true, "x"),
        fx.nav_answer(bob, view, 1, true),
        fx.notify_permission(bob, view, "https://example.com", allow),
        fx.set_blocking(bob, view, false, &["example.com"]),
        fx.blocking_compile(bob, view, "list", b"[]"),
        fx.blocking_load(bob, view, "list"),
        fx.blocking_forget(bob, view, "list"),
        fx.load_page(bob, view, b"<p>", "about:blank"),
        fx.attach_slot(bob, view, 0, 64).0,
        fx.release_slot(bob, view, 0),
        fx.resize_view(bob, view, 8, 8),
        fx.mouse(bob, view, 1),
        fx.key(bob, view, true),
    ];
    for (n, result) in calls.into_iter().enumerate() {
        assert_eq!(result, Err(ENOENT), "call {n}");
    }
    // A view that never existed answers the same.
    assert_eq!(fx.load_url(bob, view + 1000, "about:blank"), Err(ENOENT));
    assert!(
        fx.engine.take().is_empty(),
        "the engine heard of Bob's calls"
    );
    assert_eq!(fx.fabric.open_handles(), before, "Bob's buffers were kept");
    // Bob's destroy is a no-op; Alice's view works.
    fx.destroy_view(bob, view).unwrap();
    assert_eq!(fx.service.view_count(), 1);
    assert!(fx.load_url(alice, view, "about:blank").is_ok());
    // A caller in another login session is refused whatever it asks.
    let stranger = Origin {
        session: support::SESSION + 1,
        ..alice
    };
    assert_eq!(fx.load_url(stranger, view, "about:blank"), Err(EACCES));
}

#[test]
fn events_go_out_as_the_interface_the_view_was_made_through() {
    // Why: a client decodes a channel's events by the interface id of each message; a full view's
    // `Frame` has more fields than a light view's, and a client that mistook one for the other
    // would misread slots.
    let mut fx = Fx::new("event-interface");
    let app = origin(7);
    let (full, full_events) = fx.view(app, Via::View, data_only(), 4, 4);
    let (lite, lite_events) = fx.rig.create_plain(app, 4, 4);
    fx.attach_frames(app, full, 4, 4);
    for slot in 0..3 {
        fx.rig.attach(app, lite, slot, 4, 4);
    }
    let page = PageNow {
        title: "t".into(),
        finished: 1,
        ..PageNow::default()
    };
    fx.queue.page_state(full, page);
    fx.queue.state(lite, "t", false, 1, "");
    for view in [full, lite] {
        fx.rig.paint(view, 4, 4, 9);
    }
    fx.pump(0);
    fx.pump(1000);
    let tagged = fx.fabric.take_tagged(full_events);
    assert!(tagged.len() >= 3, "state, frame and a ping");
    assert!(tagged
        .iter()
        .all(|(interface, ..)| *interface == wire::INTERFACE_ID));
    let tagged = fx.fabric.take_tagged(lite_events);
    assert!(tagged.len() >= 3, "state, frame and a ping");
    assert!(tagged
        .iter()
        .all(|(interface, ..)| *interface == light::INTERFACE_ID));
}

#[test]
fn a_user_holds_a_ceiling_of_web_processes_one_per_client_and_profile() {
    // Why: a web process costs about 107 MiB under one user memory quota; the processes are the
    // distinct (client, profile, process key) triples among a user's full views.
    let mut fx = Fx::new("web-process-cap");
    let apps: Vec<Origin> = (1..=WEB_PROCESSES_PER_USER as u32 + 2)
        .map(origin)
        .collect();
    let mut first = Vec::new();
    for app in &apps[..WEB_PROCESSES_PER_USER] {
        first.push(fx.view(*app, Via::Net, web(), 4, 4));
    }
    let handles = fx.fabric.open_handles();
    assert_eq!(
        fx.create_view(apps[WEB_PROCESSES_PER_USER], Via::Net, web(), 4, 4),
        Err(EDQUOT)
    );
    assert_eq!(
        fx.fabric.open_handles(),
        handles,
        "a refused view kept its endpoint"
    );

    // A second view of a pair that has its process shares it.
    assert!(fx.create_view(apps[0], Via::Net, web(), 4, 4).is_ok());
    // Another profile of the same client is another process, in memory or on disk.
    assert_eq!(
        fx.create_view(apps[0], Via::Browser, browser(), 4, 4),
        Err(EDQUOT)
    );
    let private = wire::ViewConfig {
        ephemeral: true,
        ..browser()
    };
    assert_eq!(
        fx.create_view(apps[0], Via::Browser, private, 4, 4),
        Err(EDQUOT)
    );
    // Light views are not counted, nor do they count a pair.
    let light_config = light::LightConfig::default();
    assert!(fx
        .rig
        .create(apps[WEB_PROCESSES_PER_USER], light_config, 4, 4)
        .is_ok());
    // Another user has a cap of its own.
    let other_user = Origin {
        uid: 1001,
        ..origin(1)
    };
    assert!(fx.create_view(other_user, Via::Net, web(), 4, 4).is_ok());

    // A view the engine gave up holds no process.
    fx.queue
        .view_lost(first[WEB_PROCESSES_PER_USER - 1].0, LOST_CRASHED);
    fx.pump(0);
    assert!(fx
        .create_view(apps[WEB_PROCESSES_PER_USER], Via::Net, web(), 4, 4)
        .is_ok());
    // ...so its owner's next view of the pair is a new process, and over the cap again.
    assert_eq!(
        fx.create_view(apps[WEB_PROCESSES_PER_USER - 1], Via::Net, web(), 4, 4),
        Err(EDQUOT)
    );
    // Ending a client's last view of a pair frees the process.
    fx.destroy_view(apps[1], first[1].0).unwrap();
    assert!(fx
        .create_view(apps[WEB_PROCESSES_PER_USER - 1], Via::Net, web(), 4, 4)
        .is_ok());
    assert_eq!(fx.fabric.double_closes(), 0);
}

#[test]
fn the_view_count_per_client_is_shared_by_light_and_full_views() {
    // Why: the per-client count is shared by both kinds of view (a lost view still counts until
    // it is destroyed).
    let mut fx = Fx::new("view-count");
    let app = origin(7);
    for _ in 0..4 {
        fx.view(app, Via::View, data_only(), 2, 2);
        fx.rig.create_plain(app, 2, 2);
    }
    assert_eq!(fx.service.client_views(app.client()), 8);
    let handles = fx.fabric.open_handles();
    let full = fx.create_view(app, Via::View, data_only(), 2, 2);
    assert_eq!(full, Err(ENOSPC));
    let light_config = light::LightConfig::default();
    assert!(fx.rig.create(app, light_config, 2, 2).is_err());
    assert_eq!(fx.fabric.open_handles(), handles);
}
