//! Content blocking of a full view: the profile's switch and exceptions, the one-shot bypass of a
//! reload, and the rule lists a client compiles into the profile's store.

#![cfg(not(feature = "host"))]

mod support;

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, data_only, web, FEv, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::{EXCEPTIONS_MAX, EXCEPTION_BYTES_MAX, LIST_NAME_BYTES_MAX, RULES_BYTES_MAX};
use xui_webd::recording::Call as EngineCall;

fn blocker() -> wire::ViewConfig {
    wire::ViewConfig {
        blocking: true,
        ..browser()
    }
}

const ADS: &str = "http://ads.example/banner.js";

#[test]
fn blocking_is_on_by_default_and_follows_the_switch_and_the_exceptions() {
    // Why: `blocking(view, url)` is asked by the engine's thread for every main-frame load; the
    // client's `SetBlocking` is the only thing that changes the answer.
    let mut fx = Fx::new("blocking-settings");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    assert!(fx.queue.blocking(view, ADS));

    fx.set_blocking(app, view, false, &[]).unwrap();
    assert!(
        !fx.queue.blocking(view, ADS),
        "switched off for the profile"
    );

    // Hosts are compared in lower case, a listed host covers its subdomains and no other host
    // that merely ends in the same letters.
    fx.set_blocking(app, view, true, &["Example.COM", "news.test"])
        .unwrap();
    for (url, blocks) in [
        ("http://example.com/", false),
        ("https://www.example.com:8443/x", false),
        ("http://NEWS.test/", false),
        ("http://notexample.com/", true),
        ("http://example.com.evil.test/", true),
        ("http://other.org/", true),
        // An address with no host (a data: page) is blocked like any other: the lists decide.
        ("data:text/html,x", true),
    ] {
        assert_eq!(fx.queue.blocking(view, url), blocks, "{url}");
    }
    // The settings are the profile's: another view of it sees them.
    let (same, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    assert!(!fx.queue.blocking(same, "http://example.com/"));
    assert!(fx.queue.blocking(same, "http://example.com.evil.test/"));
    // A private window has no folder for the rule lists: blocking is refused at CreateView.
    let private = wire::ViewConfig {
        ephemeral: true,
        ..blocker()
    };
    assert!(fx
        .create_view(origin(8), Via::Browser, private, 4, 4)
        .is_err());
}

#[test]
fn exceptions_are_host_names_and_bounded() {
    // Why: they are compared against the host of every page and kept for the life of webd.
    let mut fx = Fx::new("blocking-exceptions");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    for bad in ["", "a b", "a/b", "ex\u{e4}mple.com", "a:80"] {
        assert_eq!(
            fx.set_blocking(app, view, true, &[bad]),
            Err(EINVAL),
            "{bad:?}"
        );
    }
    let long = "a".repeat(EXCEPTION_BYTES_MAX + 1);
    assert_eq!(
        fx.set_blocking(app, view, true, &[long.as_str()]),
        Err(EINVAL)
    );
    assert!(fx.set_blocking(app, view, true, &[&long[1..]]).is_ok());
    let many = vec!["a.test"; EXCEPTIONS_MAX + 1];
    assert_eq!(fx.set_blocking(app, view, true, &many), Err(EINVAL));
    assert!(fx.set_blocking(app, view, true, &many[1..]).is_ok());
    // A refused call changes nothing: the last good settings are still in force.
    fx.set_blocking(app, view, true, &["keep.test"]).unwrap();
    assert_eq!(fx.set_blocking(app, view, false, &["a b"]), Err(EINVAL));
    assert!(!fx.queue.blocking(view, "http://keep.test/"));
    assert!(fx.queue.blocking(view, ADS));
}

#[test]
fn a_reload_can_bypass_the_blocker_once() {
    // Why: "Reload without blocking" must lift the lists for one page load, not for good.
    let mut fx = Fx::new("blocking-bypass");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    let (plain, _) = fx.view(
        origin(8),
        Via::Browser,
        wire::ViewConfig {
            ephemeral: true,
            ..browser()
        },
        4,
        4,
    );
    fx.engine.take();
    fx.reload(app, view, false).unwrap();
    assert!(
        fx.queue.blocking(view, ADS),
        "an ordinary reload keeps the blocker"
    );
    // The bypass is for the page on show (its reload), as the engine last said it.
    fx.queue.page_state(
        view,
        xui_webd::events::PageNow {
            committed_url: ADS.into(),
            ..Default::default()
        },
    );
    fx.pump(0);
    fx.reload(app, view, true).unwrap();
    assert!(!fx.queue.blocking(view, ADS), "the bypassed load");
    assert!(fx.queue.blocking(view, ADS), "once");
    // A view made without blocking has nothing to bypass and nothing to block.
    fx.reload(origin(8), plain, true).unwrap();
    assert!(!fx.queue.blocking(plain, ADS));
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::Reload(view),
            EngineCall::Reload(view),
            EngineCall::Reload(plain),
        ]
    );
}

#[test]
fn a_view_made_without_blocking_cannot_change_or_feed_the_blocker() {
    // Why: the lists are the profile's, shared by its views; a view that did not ask for blocking
    // must not be able to switch it off for the others or load rules into the store.
    let mut fx = Fx::new("blocking-denied");
    let app = origin(7);
    let (blocking, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    let (view, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    fx.engine.take();
    let handles = fx.fabric.open_handles();
    assert_eq!(fx.set_blocking(app, view, false, &[]), Err(EACCES));
    assert_eq!(fx.blocking_compile(app, view, "easy", b"[]"), Err(EACCES));
    assert_eq!(fx.blocking_load(app, view, "easy"), Err(EACCES));
    assert_eq!(fx.blocking_forget(app, view, "easy"), Err(EACCES));
    assert!(fx.engine.take().is_empty());
    assert_eq!(
        fx.fabric.open_handles(),
        handles,
        "the refused rule buffer was kept"
    );
    assert!(fx.queue.blocking(blocking, ADS), "the blocker is still on");
    assert!(!fx.queue.blocking(view, ADS), "the view did not ask for it");
    // Not even for the light-weight own profile: it cannot be asked for there at all.
    let own = wire::ViewConfig {
        blocking: true,
        ..data_only()
    };
    assert!(fx.create_view(app, Via::Browser, own, 4, 4).is_err());
    let own = wire::ViewConfig {
        blocking: true,
        ..web()
    };
    assert_eq!(fx.create_view(app, Via::Browser, own, 4, 4), Err(EINVAL));
}

#[test]
fn rule_lists_reach_the_engine_intact_by_name() {
    // Why: the JSON is the whole of a content rule list, compiled by the engine into the profile's
    // store; every byte and the list's name must arrive as sent.
    let mut fx = Fx::new("blocking-lists");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, blocker(), 4, 4);
    fx.engine.take();
    let handles = fx.fabric.open_handles();
    let json: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    fx.blocking_compile(app, view, "easy-list_1.0", &json)
        .unwrap();
    // A range inside a larger buffer is the list.
    let (handle, _) = fx.fabric.new_buffer_with(b"##[{\"a\":1}]##".to_vec());
    let range = Buffer {
        handle,
        offset: 2,
        len: 9,
    };
    fx.blocking_compile_buffer(app, view, "ranged", range)
        .unwrap();
    fx.blocking_load(app, view, "easy-list_1.0").unwrap();
    fx.blocking_forget(app, view, "easy-list_1.0").unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::BlockingCompile {
                view,
                list: "easy-list_1.0".into(),
                json
            },
            EngineCall::BlockingCompile {
                view,
                list: "ranged".into(),
                json: b"[{\"a\":1}]".to_vec()
            },
            EngineCall::BlockingLoad(view, "easy-list_1.0".into()),
            EngineCall::BlockingForget(view, "easy-list_1.0".into()),
        ]
    );
    assert_eq!(fx.fabric.open_handles(), handles, "read buffers are closed");

    // The engine's answer and the count of blocked loads are events of the view.
    fx.queue
        .blocking_list(view, "easy-list_1.0", wire::BLOCKING_STATUS_READY as i32);
    fx.queue.blocked(view, 5);
    fx.queue.blocking_list(view, "ranged", -1);
    fx.pump(0);
    let sent = fx.news(events);
    assert!(sent.contains(&FEv::Blocked(wire::BlockedArgs { view, count: 5 })));
    assert!(sent.contains(&FEv::BlockingList(wire::BlockingListArgs {
        view,
        list: "easy-list_1.0".into(),
        status: wire::BLOCKING_STATUS_READY,
    })));
    // A status that is not a count at all (negative) is reported as unavailable.
    assert!(sent.contains(&FEv::BlockingList(wire::BlockingListArgs {
        view,
        list: "ranged".into(),
        status: wire::BLOCKING_STATUS_UNAVAILABLE,
    })));
}

#[test]
fn list_names_are_short_and_plain_and_a_list_is_at_most_32_mib() {
    // Why: the name becomes a file name in the profile's rule store; the size is checked on the
    // declared range before the buffer is read.
    let mut fx = Fx::new("blocking-names");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, blocker(), 4, 4);
    fx.engine.take();
    let handles = fx.fabric.open_handles();
    let longest = "a".repeat(LIST_NAME_BYTES_MAX);
    let too_long = "a".repeat(LIST_NAME_BYTES_MAX + 1);
    for bad in [
        "",
        "a b",
        "../x",
        "a/b",
        "a\\b",
        "a:b",
        "l\u{e9}",
        too_long.as_str(),
    ] {
        assert_eq!(
            fx.blocking_compile(app, view, bad, b"[]"),
            Err(EINVAL),
            "{bad:?}"
        );
        assert_eq!(fx.blocking_load(app, view, bad), Err(EINVAL), "{bad:?}");
        assert_eq!(fx.blocking_forget(app, view, bad), Err(EINVAL), "{bad:?}");
    }
    assert!(fx.blocking_compile(app, view, &longest, b"[]").is_ok());
    assert!(fx.blocking_compile(app, view, "a.b-c_D9", b"[]").is_ok());
    assert_eq!(
        fx.engine.take().len(),
        2,
        "only the good names reached the engine"
    );

    // Over the cap on the declared range: refused without being read, and closed.
    let (handle, _) = fx.fabric.new_buffer(16);
    let huge = Buffer::whole(handle, RULES_BYTES_MAX + 1);
    assert_eq!(
        fx.blocking_compile_buffer(app, view, "huge", huge),
        Err(E2BIG)
    );
    let (handle, _) = fx.fabric.new_buffer(16);
    let wrapped = Buffer::whole(handle, u64::MAX);
    assert!(fx
        .blocking_compile_buffer(app, view, "wrapped", wrapped)
        .is_err());
    // At the cap, on a buffer that is smaller, the range does not fit: refused, not read past it.
    let (handle, _) = fx.fabric.new_buffer(16);
    let short = Buffer::whole(handle, RULES_BYTES_MAX);
    assert_eq!(
        fx.blocking_compile_buffer(app, view, "short", short),
        Err(EINVAL)
    );
    assert!(fx.engine.take().is_empty());
    assert_eq!(fx.fabric.open_handles(), handles);
    assert_eq!(fx.fabric.double_closes(), 0);
}
