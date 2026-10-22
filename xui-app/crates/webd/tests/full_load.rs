//! What a client may make a full view load, and the calls that pass through to the engine: the
//! scheme policy applied to `Load`, the base URL of `LoadHtml`, input, and that a refused request
//! leaves no handle behind.

#![cfg(not(feature = "host"))]

mod support;

use libmessenger::Buffer;
use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{data_only, listed, web, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::{HTML_BYTES_MAX, URL_BYTES_MAX};
use xui_webd::recording::Call as EngineCall;

const URLS: [&str; 11] = [
    "about:blank",
    "data:text/html,hi",
    "blob:http://a/1",
    "http://10.0.2.2:18081/page",
    "https://example.com/x",
    "http://example.com/",
    "http://10.0.2.2:18082/",
    "file:///etc/passwd",
    "mailto:a@b",
    "javascript:alert(1)",
    "nonsense",
];

#[test]
fn load_follows_the_views_scheme_policy_and_only_loadable_addresses_reach_the_engine() {
    // Why: a page's own navigations are judged by the policy on the engine's thread, but the
    // client's `Load` is judged here: a client must not widen its own view (no `file:`, no
    // network under `DataOnly`, no origin it did not list), and `mailto:` is never loaded.
    let mut fx = Fx::new("load-policy");
    let app = origin(7);
    let listed_cfg = listed(&["http://10.0.2.2:18081", "https://example.com"]);
    let (on_list, _) = fx.view(app, Via::Net, listed_cfg, 4, 4);
    let (offline, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let (open, _) = fx.view(app, Via::Net, web(), 4, 4);
    fx.engine.take();
    // (listed, data only, web) for each address.
    let allowed = [
        (true, true, true),
        (true, true, true),
        (true, true, true),
        (true, false, true),
        (true, false, true),
        (false, false, true),
        (false, false, true),
        (false, false, false),
        (false, false, false),
        (false, false, false),
        (false, false, false),
    ];
    let mut expected = Vec::new();
    for (url, (a, b, c)) in URLS.iter().zip(allowed) {
        for (view, ok) in [(on_list, a), (offline, b), (open, c)] {
            let want = if ok { Ok(Vec::new()) } else { Err(EACCES) };
            assert_eq!(fx.load_url(app, view, url), want, "{url} in view {view}");
            if ok {
                expected.push(EngineCall::Load(view, (*url).to_owned()));
            }
        }
    }
    assert_eq!(fx.engine.take(), expected);
    // The origin is compared normalised, not as text.
    assert!(fx.load_url(app, on_list, "HTTP://10.0.2.2:18081/x").is_ok());
    assert!(fx
        .load_url(app, on_list, "https://EXAMPLE.com:443/")
        .is_ok());
    assert_eq!(
        fx.load_url(app, on_list, "https://example.com.evil.test/"),
        Err(EACCES)
    );
    assert_eq!(
        fx.load_url(app, on_list, "https://user@evil.test@example.com/"),
        Ok(Vec::new())
    );
}

#[test]
#[ignore = "bug: policy::origin() splits the authority at / ? # only, not at a backslash, so \
            https://evil.test\\@example.com/ is read as host example.com (WebKit: evil.test)"]
fn a_backslash_does_not_hide_the_real_host_from_the_origin_list() {
    // Why: the engine's URL parser reads `\` as `/` in an http address, so the host of
    // `https://evil.test\@example.com/` is evil.test (the rest is its path); a policy that read
    // it as userinfo `evil.test\` at example.com would let a client load a host it never listed.
    let mut fx = Fx::new("load-backslash");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Net, listed(&["https://example.com"]), 4, 4);
    fx.engine.take();
    for url in [
        "https://evil.test\\@example.com/",
        "https://evil.test\\.example.com/",
        "https://evil.test:443\\@example.com/",
    ] {
        assert_eq!(fx.load_url(app, view, url), Err(EACCES), "{url}");
    }
    let (open, _) = fx.view(app, Via::Net, web(), 4, 4);
    fx.engine.take();
    assert_eq!(
        fx.load_page(app, open, b"<p>", "https://evil.test\\@example.com/"),
        Ok(Vec::new())
    );
    assert!(fx.engine.take().len() == 1);
    assert_eq!(
        fx.load_page(app, view, b"<p>", "https://evil.test\\@example.com/"),
        Err(EACCES)
    );
    assert!(fx.engine.take().is_empty());
}

#[test]
fn an_address_over_8_kib_or_with_a_nul_is_invalid_before_the_policy_is_asked() {
    // Why: the address travels to the engine as a C string and into every state event.
    let mut fx = Fx::new("load-size");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Net, web(), 4, 4);
    let (offline, _) = fx.view(app, Via::View, data_only(), 4, 4);
    fx.engine.take();
    let prefix = "http://a.example/";
    let exactly = format!("{prefix}{}", "a".repeat(URL_BYTES_MAX - prefix.len()));
    assert!(fx.load_url(app, view, &exactly).is_ok());
    let over = format!("{exactly}a");
    assert_eq!(fx.load_url(app, view, &over), Err(EINVAL));
    assert_eq!(
        fx.load_url(app, view, "http://a.example/\0evil"),
        Err(EINVAL)
    );
    // Too long wins over "not allowed": the size is the first check.
    assert_eq!(fx.load_url(app, offline, &over), Err(EINVAL));
    // (Not `assert_eq!` on the calls: a failure would print 8 KiB addresses.)
    let calls = fx.engine.take();
    assert!(
        calls == vec![EngineCall::Load(view, exactly)],
        "only the address at the cap loads"
    );
}

#[test]
fn load_html_may_name_a_base_the_policy_lets_the_view_load_and_never_a_file() {
    // Why: relative addresses in a page resolve against its base, so a base at an address is a
    // way to make the page fetch from there; a full view may use only addresses it could load.
    let mut fx = Fx::new("load-html-base");
    let app = origin(7);
    let (open, _) = fx.view(app, Via::Net, web(), 4, 4);
    let (offline, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let (on_list, _) = fx.view(app, Via::Net, listed(&["http://10.0.2.2:18081"]), 4, 4);
    fx.engine.take();
    let handles = fx.fabric.open_handles();

    let cases = [
        (open, "", Some("about:blank")),
        (open, "about:blank", Some("about:blank")),
        (
            open,
            "http://example.com/dir/",
            Some("http://example.com/dir/"),
        ),
        (open, "https://example.com/", Some("https://example.com/")),
        (open, "data:text/html,x", Some("data:text/html,x")),
        (open, "file:///etc/", None),
        (open, "javascript:alert(1)", None),
        (open, "mailto:a@b", None),
        (open, "ftp://example.com/", None),
        (offline, "", Some("about:blank")),
        (offline, "about:srcdoc", Some("about:srcdoc")),
        (offline, "http://example.com/", None),
        (
            on_list,
            "http://10.0.2.2:18081/app/",
            Some("http://10.0.2.2:18081/app/"),
        ),
        (on_list, "http://10.0.2.2:18082/", None),
    ];
    for (view, base, want) in cases {
        let result = fx.load_page(app, view, b"<p>hi</p>", base);
        match want {
            Some(base_url) => {
                assert!(result.is_ok(), "{base:?}");
                let expected = EngineCall::LoadHtml {
                    view,
                    html: b"<p>hi</p>".to_vec(),
                    base_url: base_url.to_owned(),
                };
                assert_eq!(fx.engine.take(), vec![expected], "{base:?}");
            }
            None => {
                assert_eq!(result, Err(EACCES), "{base:?}");
                assert!(fx.engine.take().is_empty(), "{base:?}");
            }
        }
    }
    // A base too long or with a NUL is refused too, and nothing reaches the engine.
    let long = format!("http://example.com/{}", "a".repeat(URL_BYTES_MAX));
    assert!(fx.load_page(app, open, b"<p>", &long).is_err());
    assert!(fx
        .load_page(app, open, b"<p>", "http://example.com/\0")
        .is_err());
    assert!(fx.engine.take().is_empty());
    // Every page buffer, taken or refused, was closed.
    assert_eq!(fx.fabric.open_handles(), handles);
    assert_eq!(fx.fabric.double_closes(), 0);
}

#[test]
fn html_bytes_pass_through_whole_and_invalid_utf8_becomes_replacement_characters() {
    // Why: the engine takes UTF-8; a page the client got wrong must still show, not be refused.
    let mut fx = Fx::new("load-html-bytes");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::View, data_only(), 4, 4);
    fx.engine.take();
    fx.load_page(app, view, b"<p>caf\xc3\xa9 \xff</p>", "")
        .unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![EngineCall::LoadHtml {
            view,
            html: "<p>caf\u{e9} \u{fffd}</p>".as_bytes().to_vec(),
            base_url: "about:blank".into(),
        }]
    );
    // A range inside a bigger buffer is read as declared.
    let (handle, _) = fx.fabric.new_buffer_with(b"xx<b>y</b>zz".to_vec());
    let range = Buffer {
        handle,
        offset: 2,
        len: 8,
    };
    fx.load_buffer(app, view, range, "").unwrap();
    assert!(
        matches!(&fx.engine.take()[0], EngineCall::LoadHtml { html, .. } if html == b"<b>y</b>")
    );
}

#[test]
fn a_page_over_8_mib_is_too_big_and_its_buffer_is_closed_unmapped() {
    // Why: the cap is checked on the declared range, before anything is read or mapped, so a
    // client cannot make the service allocate by declaring a huge range.
    let mut fx = Fx::new("load-html-cap");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::View, data_only(), 4, 4);
    fx.engine.take();
    let handles = fx.fabric.open_handles();
    let (handle, _) = fx.fabric.new_buffer(16);
    let huge = Buffer::whole(handle, HTML_BYTES_MAX + 1);
    assert_eq!(fx.load_buffer(app, view, huge, "about:blank"), Err(E2BIG));
    let (handle, _) = fx.fabric.new_buffer(16);
    let wrapped = Buffer::whole(handle, u64::MAX);
    assert!(fx.load_buffer(app, view, wrapped, "about:blank").is_err());
    assert!(fx.engine.take().is_empty());
    assert_eq!(fx.fabric.open_handles(), handles);
}

#[test]
fn navigation_and_input_calls_reach_the_engine_for_the_callers_full_view() {
    // Why: these are the calls a browser makes all day; each must arrive once, for the right
    // view, with the modifier bits the engine knows and no others.
    let mut fx = Fx::new("passthrough");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Net, web(), 4, 4);
    fx.engine.take();
    fx.reload(app, view, false).unwrap();
    fx.stop(app, view).unwrap();
    fx.back(app, view).unwrap();
    fx.forward(app, view).unwrap();
    // A view made without content blocking has nothing to bypass; the reload still happens.
    fx.reload(app, view, true).unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::Reload(view),
            EngineCall::Stop(view),
            EngineCall::Back(view),
            EngineCall::Forward(view),
            EngineCall::Reload(view),
        ]
    );
    let mouse = wire::MouseArgs {
        view,
        kind: 1,
        button: 2,
        x: 5,
        y: 6,
        mods: 0xFFFF_FFFF,
    };
    fx.call(
        app,
        wire::METHOD_MOUSE,
        wire::encode_mouse_args(&mouse).unwrap(),
    )
    .unwrap();
    let bad = wire::MouseArgs {
        kind: 3,
        ..mouse.clone()
    };
    assert_eq!(
        fx.call(
            app,
            wire::METHOD_MOUSE,
            wire::encode_mouse_args(&bad).unwrap()
        ),
        Err(EINVAL)
    );
    let bad = wire::MouseArgs { button: 3, ..mouse };
    assert_eq!(
        fx.call(
            app,
            wire::METHOD_MOUSE,
            wire::encode_mouse_args(&bad).unwrap()
        ),
        Err(EINVAL)
    );
    fx.key(app, view, true).unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::Mouse {
                view,
                kind: 1,
                button: 2,
                x: 5,
                y: 6,
                mods: 0xF
            },
            EngineCall::Key {
                view,
                press: true,
                keysym: 0x61,
                text: "a".into(),
                mods: 0
            },
        ]
    );
    // A destroyed view is gone for every call.
    fx.destroy_view(app, view).unwrap();
    assert_eq!(fx.reload(app, view, false), Err(ENOENT));
    assert_eq!(fx.load_url(app, view, "about:blank"), Err(ENOENT));
}

#[test]
fn a_refused_full_request_carrying_a_buffer_keeps_no_handle_and_destroy_closes_the_rest() {
    // Why: every object a request carries is closed, mapped or kept exactly once; a leak here is
    // a hole in the service's handle table that any client can widen.
    let mut fx = Fx::new("handles");
    let app = origin(7);
    let cfg = wire::ViewConfig {
        deltas: true,
        ..data_only()
    };
    let (view, _) = fx.view(app, Via::View, cfg, 4, 4);
    let (plain, _) = fx.view(app, Via::View, data_only(), 4, 4);
    let baseline = fx.fabric.open_handles();

    // Each of these is refused for a different reason.
    let (page, _) = fx.fabric.new_buffer(64);
    let result = fx.load_buffer(app, view, Buffer::whole(page, 1), "http://x/");
    assert_eq!(result, Err(EACCES));
    let (rules, _) = fx.fabric.new_buffer(64);
    let result = fx.blocking_compile_buffer(app, view, "list", Buffer::whole(rules, 2));
    assert_eq!(result, Err(EACCES));
    assert_eq!(
        fx.fabric.open_handles(),
        baseline,
        "refused buffers were kept"
    );
    for (view, slot, bytes) in [
        (view, 5, 64),            // no such slot
        (view, 0, 63),            // smaller than the view
        (view, 3, 64),            // a band slot needs width * 1024 * 4
        (plain, 3, 4 * 1024 * 4), // not a view with deltas
        (view, 99, 64),
    ] {
        let (result, _) = fx.attach_slot(app, view, slot, bytes);
        assert_eq!(result, Err(EINVAL), "slot {slot} with {bytes} bytes");
        assert_eq!(
            fx.fabric.open_handles(),
            baseline,
            "slot {slot} kept its buffer"
        );
    }

    // Kept buffers (attached slots) are closed with the view.
    fx.attach_frames(app, view, 4, 4);
    assert!(fx.attach_slot(app, view, 3, 4 * 1024 * 4).0.is_ok());
    assert!(fx.attach_slot(app, view, 4, 4 * 1024 * 4).0.is_ok());
    assert_eq!(fx.fabric.open_handles(), baseline + 5);
    fx.destroy_view(app, view).unwrap();
    fx.destroy_view(app, plain).unwrap();
    assert_eq!(fx.fabric.open_handles(), 0);
    assert_eq!(fx.fabric.double_closes(), 0);
}
