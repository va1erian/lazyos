//! Who may make which full view: the scope an interface stands for (what the view may reach), what
//! the engine is told, and that a scope interface makes views and nothing else. The walls between
//! views and clients are in `full_isolation.rs`; the page's behaviour in `full_load.rs`,
//! `full_questions.rs`, `full_downloads.rs` and `full_frames.rs`.

#![cfg(not(feature = "host"))]

mod support;

use std::path::Path;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, data_only, listed, web, Fx, Via};
use support::origin;
use xui_webd::engine::{
    FullConfig, FEATURE_BLOCKING, FEATURE_DIALOGS, FEATURE_DOWNLOADS, FEATURE_NOTIFICATIONS,
    FEATURE_PAGE_UI, FEATURE_POLICY,
};
use xui_webd::fabric::errno::*;
use xui_webd::ident::Origin;
use xui_webd::recording::Call as EngineCall;

fn creates(fx: &Fx) -> usize {
    fx.engine
        .calls()
        .iter()
        .filter(|call| matches!(call, EngineCall::ViewCreate(..)))
        .count()
}

/// A `CreateView` the service must refuse with `code`, leaving the engine unaware and no handle held.
fn refused(fx: &mut Fx, who: Origin, via: Via, config: wire::ViewConfig, code: i64) {
    let (made, handles) = (creates(fx), fx.fabric.open_handles());
    assert_eq!(
        fx.create_view(who, via, config.clone(), 8, 8),
        Err(code),
        "{via:?} {config:?}"
    );
    assert_eq!(creates(fx), made, "the engine heard of a refused view");
    assert_eq!(
        fx.fabric.open_handles(),
        handles,
        "the event endpoint leaked"
    );
}

/// A `CreateView` the service must accept (the view is then ended, to leave room).
fn accepted(fx: &mut Fx, who: Origin, via: Via, config: wire::ViewConfig) {
    let (view, _) = fx
        .create_view(who, via, config.clone(), 8, 8)
        .unwrap_or_else(|code| panic!("{via:?} {config:?}: errno {code}"));
    fx.destroy_view(who, view).unwrap();
}

#[test]
fn the_view_scope_has_no_network_and_net_and_browser_have() {
    // Why: the scope is the only thing that keeps a package without the network permission from
    // loading a page from the internet; it is decided by the interface of the request, never by
    // anything in the config.
    let mut fx = Fx::new("scope-network");
    let app = origin(7);
    refused(&mut fx, app, Via::View, web(), EACCES);
    refused(
        &mut fx,
        app,
        Via::View,
        listed(&["http://example.com"]),
        EACCES,
    );
    accepted(&mut fx, app, Via::View, data_only());
    for via in [Via::Net, Via::Browser] {
        accepted(&mut fx, app, via, web());
        accepted(&mut fx, app, via, listed(&["http://example.com"]));
        accepted(&mut fx, app, via, data_only());
    }
    assert_eq!(fx.service.view_count(), 0);
}

#[test]
fn the_browser_profile_downloads_blocking_and_hand_offs_need_the_browser_scope() {
    // Why: the browser profile is the person's cookies and history, kept on disk, and downloads
    // write into their folder: only a package granted `browser.v1` may reach either.
    let mut fx = Fx::new("scope-browser");
    let app = origin(7);
    let on_disk = |config: wire::ViewConfig| wire::ViewConfig {
        profile: "browser".into(),
        ..config
    };
    for via in [Via::View, Via::Net] {
        refused(&mut fx, app, via, on_disk(web()), EACCES);
    }
    refused(&mut fx, app, Via::View, on_disk(data_only()), EACCES);
    accepted(&mut fx, app, Via::Browser, on_disk(web()));
    let ephemeral = wire::ViewConfig {
        ephemeral: true,
        ..on_disk(web())
    };
    accepted(&mut fx, app, Via::Browser, ephemeral);
    refused(
        &mut fx,
        app,
        Via::Browser,
        wire::ViewConfig {
            profile: "work".into(),
            ..web()
        },
        EINVAL,
    );

    let browse = wire::ViewConfig {
        handoff: wire::HANDOFF_BROWSER,
        ..data_only()
    };
    refused(&mut fx, app, Via::View, browse.clone(), EACCES);
    refused(
        &mut fx,
        app,
        Via::Net,
        wire::ViewConfig {
            handoff: wire::HANDOFF_BROWSER,
            ..web()
        },
        EACCES,
    );
    accepted(&mut fx, app, Via::Browser, browse);

    for (downloads, blocking) in [(true, false), (false, true)] {
        let wants = wire::ViewConfig {
            downloads,
            blocking,
            ..on_disk(web())
        };
        refused(&mut fx, app, Via::Net, wants.clone(), EACCES);
        accepted(&mut fx, app, Via::Browser, wants.clone());
        // Both need the profile on disk: the app's own profile has no list store or folder.
        let own = wire::ViewConfig {
            profile: String::new(),
            ..wants
        };
        refused(&mut fx, app, Via::Browser, own, EINVAL);
    }
    // Blocking is for pages from the network; a view with none has nothing to block.
    let silly = wire::ViewConfig {
        blocking: true,
        ..on_disk(data_only())
    };
    refused(&mut fx, app, Via::Browser, silly, EINVAL);
}

#[test]
fn notifications_need_a_profile_kept_on_disk() {
    // Why: WebKit refuses a notification permission request in a page whose session is not
    // persistent, so the view would hang waiting; the service refuses it up front.
    let mut fx = Fx::new("notify-config");
    let app = origin(7);
    let asks = |config: wire::ViewConfig| wire::ViewConfig {
        notifications: true,
        ..config
    };
    refused(&mut fx, app, Via::Browser, asks(web()), EINVAL);
    refused(&mut fx, app, Via::Browser, asks(data_only()), EINVAL);
    let private = wire::ViewConfig {
        ephemeral: true,
        ..browser()
    };
    refused(&mut fx, app, Via::Browser, asks(private), EINVAL);
    refused(&mut fx, app, Via::Net, asks(browser()), EACCES);
    accepted(&mut fx, app, Via::Browser, asks(browser()));
}

#[test]
fn malformed_configs_and_sizes_are_refused_before_the_engine_hears() {
    // Why: every field of a ViewConfig reaches a C string or a table in the engine's process.
    let mut fx = Fx::new("config-bad");
    let app = origin(7);
    let bad = [
        wire::ViewConfig {
            scheme_policy: 7,
            ..data_only()
        },
        wire::ViewConfig {
            handoff: 9,
            ..data_only()
        },
        wire::ViewConfig {
            user_agent_suffix: "x".repeat(65),
            ..data_only()
        },
        wire::ViewConfig {
            user_agent_suffix: "caf\u{e9}".into(),
            ..data_only()
        },
        wire::ViewConfig {
            user_agent_suffix: "a\nb".into(),
            ..data_only()
        },
        wire::ViewConfig {
            origins: vec!["ftp://example.com".into()],
            ..listed(&[])
        },
        wire::ViewConfig {
            origins: vec!["http://".into()],
            ..listed(&[])
        },
        wire::ViewConfig {
            origins: vec![format!("http://{}", "a".repeat(260))],
            ..listed(&[])
        },
        wire::ViewConfig {
            origins: vec!["http://a.example".into(); 65],
            ..listed(&[])
        },
    ];
    for config in bad {
        refused(&mut fx, app, Via::Net, config, EINVAL);
    }
    accepted(
        &mut fx,
        app,
        Via::Net,
        wire::ViewConfig {
            user_agent_suffix: "x".repeat(64),
            ..web()
        },
    );
    accepted(
        &mut fx,
        app,
        Via::Net,
        wire::ViewConfig {
            origins: vec!["http://a.example".into(); 64],
            ..listed(&[])
        },
    );
    let (made, handles) = (creates(&fx), fx.fabric.open_handles());
    for (w, h) in [(0, 5), (5, 0), (8193, 1), (4097, 4096)] {
        assert_eq!(fx.create_view(app, Via::Net, web(), w, h), Err(EINVAL));
    }
    assert_eq!((creates(&fx), fx.fabric.open_handles()), (made, handles));
}

#[test]
fn the_engine_gets_the_config_the_scope_and_the_profile_decide() {
    // Why: the engine trusts the service completely; the bits it is handed are the whole sandbox
    // of the page (features, profile folder, memory-only store, no-network lock).
    let mut fx = Fx::new("engine-config");
    let app = origin(7);
    let everything = wire::ViewConfig {
        scripts: true,
        popups: true,
        dialogs: true,
        downloads: true,
        notifications: true,
        blocking: true,
        deltas: true,
        user_agent_suffix: "LazyTest/1".into(),
        ..browser()
    };
    let (view, _) = fx.view(app, Via::Browser, everything, 320, 200);
    let group = fx.service.group_of(app.client()).unwrap();
    let EngineCall::ViewCreate(id, config) = fx.engine.take().remove(0) else {
        panic!("the first call is the view's creation");
    };
    assert_eq!(id, view);
    assert_eq!(
        (config.width, config.height, config.group),
        (320, 200, group)
    );
    assert!(config.scripts);
    let mut full = config.full.expect("a full view");
    // The folder is compared as a path: the separator is the host's.
    assert_eq!(
        Path::new(&full.profile),
        Path::new("/webd-test/profiles/browser")
    );
    full.profile.clear();
    assert_eq!(
        full,
        FullConfig {
            features: FEATURE_POLICY
                | FEATURE_PAGE_UI
                | FEATURE_DIALOGS
                | FEATURE_DOWNLOADS
                | FEATURE_NOTIFICATIONS
                | FEATURE_BLOCKING,
            profile: String::new(),
            memory_only: false,
            deltas: true,
            user_agent_suffix: "LazyTest/1".into(),
            no_network: false,
            private_store: false,
        }
    );

    // A private window: no folder (a memory store keyed by it would be every client's), its data in
    // memory and its group's own.
    let private = wire::ViewConfig {
        ephemeral: true,
        ..browser()
    };
    fx.view(app, Via::Browser, private, 64, 64);
    let EngineCall::ViewCreate(_, config) = fx.engine.take().remove(0) else {
        panic!("creation");
    };
    let full = config.full.unwrap();
    assert!(full.memory_only);
    assert!(full.private_store, "not the client's own in-memory store");
    assert_eq!(full.features, FEATURE_POLICY | FEATURE_PAGE_UI);
    assert_eq!(full.profile, "");

    // No network at all, and the app's own profile: no folder, memory, the lock on.
    fx.view(origin(8), Via::View, data_only(), 64, 64);
    let EngineCall::ViewCreate(_, config) = fx.engine.take().remove(0) else {
        panic!("creation");
    };
    let full = config.full.unwrap();
    assert_eq!(
        full,
        FullConfig {
            features: FEATURE_POLICY | FEATURE_PAGE_UI,
            profile: String::new(),
            memory_only: true,
            deltas: false,
            user_agent_suffix: String::new(),
            no_network: true,
            private_store: false,
        }
    );
}

#[test]
fn a_scope_interface_refuses_every_method_but_create_view() {
    // Why: `net.v1` and `browser.v1` are permissions to *make* views; every other call goes through
    // `view.v1`, which a package lists separately. A scope that also served Load would let a
    // package with only the scope act on views of a package without it.
    let mut fx = Fx::new("scope-methods");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    fx.engine.take();
    let destroy = wire::encode_destroy_view_args(&wire::DestroyViewArgs { view }).unwrap();
    let load = wire::encode_load_args(&wire::LoadArgs {
        view,
        url: "about:blank".into(),
    })
    .unwrap();
    for via in [Via::Net, Via::Browser] {
        let methods = [
            wire::METHOD_DESTROYVIEW,
            wire::METHOD_ATTACHSLOT,
            wire::METHOD_RELEASESLOT,
            wire::METHOD_RESIZE,
            wire::METHOD_SETVISIBLE,
            wire::METHOD_REFRESH,
            wire::METHOD_LOAD,
            wire::METHOD_LOADHTML,
            wire::METHOD_RELOAD,
            wire::METHOD_STOP,
            wire::METHOD_BACK,
            wire::METHOD_FORWARD,
            wire::METHOD_DOWNLOAD,
            wire::METHOD_RECOVER,
            wire::METHOD_MOUSE,
            wire::METHOD_WHEEL,
            wire::METHOD_KEY,
            wire::METHOD_FOCUS,
            wire::METHOD_DIALOGREPLY,
            wire::METHOD_NAVIGATIONANSWER,
            wire::METHOD_NOTIFYPERMISSION,
            wire::METHOD_DOWNLOADCANCEL,
            wire::METHOD_SETBLOCKING,
            wire::METHOD_BLOCKINGCOMPILE,
            wire::METHOD_BLOCKINGLOAD,
            wire::METHOD_BLOCKINGFORGET,
            wire::METHOD_NOTIFYFORGET,
            wire::METHOD_NOTIFYSTATE,
            wire::METHOD_FRAME,
            0,
            99,
        ];
        for method in methods {
            let body = match method {
                wire::METHOD_DESTROYVIEW => destroy.clone(),
                wire::METHOD_LOAD => load.clone(),
                _ => Vec::new(),
            };
            let result = fx.send(app, via.id(), method, body, vec![]);
            assert_eq!(result, Err(ENOTSUP), "{via:?} method {method}");
        }
        // A request that carries a buffer is refused without keeping it.
        let before = fx.fabric.open_handles();
        let (handle, _) = fx.fabric.new_buffer(64);
        let args = wire::AttachSlotArgs {
            view,
            slot: 0,
            pixels: libmessenger::Buffer::whole(handle, 64),
        };
        let (body, objects) = wire::encode_attach_slot_args(&args).unwrap();
        let result = fx.send(app, via.id(), wire::METHOD_ATTACHSLOT, body, objects);
        assert_eq!(result, Err(ENOTSUP));
        assert_eq!(fx.fabric.open_handles(), before, "the buffer was kept");
    }
    assert_eq!(fx.service.view_count(), 1, "nothing was destroyed");
    assert!(
        fx.engine.take().is_empty(),
        "the engine heard of a refused call"
    );
    assert_eq!(fx.fabric.double_closes(), 0);
}

#[test]
fn memory_pressure_refuses_a_view_that_would_start_a_new_web_process() {
    // Why: a web process is about 107 MiB; under a low user-memory quota the child's `exec` would
    // fail with ENOMEM (the old in-process browser hit this), so webd refuses the view that would
    // start a new process once the user's charge cannot hold one more (docs/webd-plan.md 11 item 7).
    let mut fx = Fx::new("mem-pressure");
    let app = origin(7);
    // 8 GiB limit, 200 MiB used: room for a process. The view is made.
    fx.set_memory(200 << 20, 8 << 30);
    let (first, _) = fx
        .create_view(app, Via::Browser, browser(), 8, 8)
        .expect("a view fits when the charge is low");

    // Now the charge is within one process' reserve of the limit: a view that would start a *new*
    // web process (a different profile) is refused with EDQUOT, the engine never hears of it, and no
    // handle leaks.
    fx.set_memory((8 << 30) - (100 << 20), 8 << 30);
    let made = creates(&fx);
    let handles = fx.fabric.open_handles();
    assert_eq!(
        fx.create_view(app, Via::Net, web(), 8, 8),
        Err(EDQUOT),
        "a new-process view must be refused under memory pressure"
    );
    assert_eq!(creates(&fx), made, "the engine heard of a refused view");
    assert_eq!(fx.fabric.open_handles(), handles, "the endpoint leaked");

    // A view that joins an existing (client, profile) group starts no process, so it is still made
    // even under pressure (its web process already exists).
    let (second, _) = fx
        .create_view(app, Via::Browser, browser(), 8, 8)
        .expect("a view that reuses an existing process is not gated by memory");
    assert_ne!(second, first);

    // When the host cannot read the charge (limit 0) the gate is off: the fixed process cap is then
    // the only bound.
    fx.set_memory(u64::MAX, 0);
    let other = origin(8);
    fx.create_view(other, Via::Net, web(), 8, 8)
        .expect("no gating when the charge is unknown");
}
