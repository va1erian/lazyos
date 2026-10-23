//! The user agent of a full view (docs/lazyweb-plan.md, a tab asking for the desktop site):
//! `ViewConfig.user_agent` and `SetUserAgent`, and which views may use them.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, web, Fx, Via};
use support::origin;
use xui_webd::engine::{EngineEvents, LOST_OTHER};
use xui_webd::fabric::errno::*;
use xui_webd::limits::USER_AGENT_MAX;
use xui_webd::recording::Call as EngineCall;

fn agent_tab(agent: &str) -> wire::ViewConfig {
    wire::ViewConfig {
        user_agent: agent.into(),
        ..browser()
    }
}

#[test]
fn a_user_agent_is_a_browser_scope_option_and_reaches_the_engine() {
    // Why: a full user agent changes how every site sees the browser, so the scope that may do it is
    // the browser's, and the engine is handed exactly the text that was checked.
    let mut fx = Fx::new("agent-config");
    let app = origin(7);
    let agent = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15";
    let (view, _) = fx.view(app, Via::Browser, agent_tab(agent), 4, 4);
    let Some(EngineCall::ViewCreate(id, config)) = fx.engine.take().into_iter().next() else {
        panic!("the view's creation");
    };
    assert_eq!(id, view);
    let full = config.full.unwrap();
    assert_eq!(full.user_agent, agent);
    assert_eq!(full.user_agent_suffix, "");
    // The other scopes may only append a suffix.
    for via in [Via::View, Via::Net] {
        let config = wire::ViewConfig {
            user_agent: "X/1".into(),
            ..if via == Via::View {
                support::full::data_only()
            } else {
                web()
            }
        };
        assert_eq!(
            fx.create_view(app, via, config, 4, 4),
            Err(EACCES),
            "{via:?}"
        );
    }
    // An agent and a suffix together are ambiguous.
    let both = wire::ViewConfig {
        user_agent_suffix: "Suffix/1".into(),
        ..agent_tab("Agent/1")
    };
    assert_eq!(fx.create_view(app, Via::Browser, both, 4, 4), Err(EINVAL));
    // Short printable ASCII only.
    for bad in [
        "x".repeat(USER_AGENT_MAX + 1),
        "caf\u{e9}".into(),
        "a\nb".into(),
    ] {
        assert_eq!(
            fx.create_view(app, Via::Browser, agent_tab(&bad), 4, 4),
            Err(EINVAL)
        );
    }
    assert!(fx
        .create_view(
            app,
            Via::Browser,
            agent_tab(&"x".repeat(USER_AGENT_MAX)),
            4,
            4
        )
        .is_ok());
}

#[test]
fn set_user_agent_changes_the_agent_of_a_view_made_for_it_and_no_other() {
    // Why: the engine's site quirks, which put a fixed agent on every request, are off only for a view
    // made with an agent or a suffix through the browser scope; elsewhere a runtime change would do
    // nothing, or widen what a scope allows.
    let mut fx = Fx::new("agent-set");
    let (app, other) = (origin(7), origin(8));
    let suffixed = wire::ViewConfig {
        user_agent_suffix: "LazyWeb/1".into(),
        ..browser()
    };
    let (with_agent, _) = fx.view(app, Via::Browser, agent_tab("Agent/1"), 4, 4);
    let (with_suffix, _) = fx.view(app, Via::Browser, suffixed.clone(), 4, 4);
    let (plain, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    // A suffix through the net scope is allowed, but not a runtime agent.
    let (net, _) = fx.view(
        app,
        Via::Net,
        wire::ViewConfig {
            user_agent_suffix: "Net/1".into(),
            ..web()
        },
        4,
        4,
    );
    fx.engine.take();
    fx.set_user_agent(app, with_agent, "Desktop/2").unwrap();
    fx.set_user_agent(app, with_suffix, "Desktop/2").unwrap();
    fx.set_user_agent(app, with_agent, "").unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::SetUserAgent(with_agent, "Desktop/2".into()),
            EngineCall::SetUserAgent(with_suffix, "Desktop/2".into()),
            EngineCall::SetUserAgent(with_agent, String::new()),
        ]
    );
    assert_eq!(fx.set_user_agent(app, plain, "Desktop/2"), Err(EACCES));
    assert_eq!(fx.set_user_agent(app, net, "Desktop/2"), Err(EACCES));
    assert_eq!(
        fx.set_user_agent(other, with_agent, "Desktop/2"),
        Err(ENOENT)
    );
    for bad in [
        "x".repeat(USER_AGENT_MAX + 1),
        "caf\u{e9}".into(),
        "a\nb".into(),
    ] {
        assert_eq!(fx.set_user_agent(app, with_agent, &bad), Err(EINVAL));
    }
    assert!(fx.call(app, wire::METHOD_SETUSERAGENT, vec![7]).is_err());
    assert!(
        fx.engine.take().is_empty(),
        "a refused call reached the engine"
    );
    fx.queue.view_lost(with_agent, LOST_OTHER);
    fx.pump(0);
    assert_eq!(fx.set_user_agent(app, with_agent, "Desktop/2"), Err(ENODEV));
}
