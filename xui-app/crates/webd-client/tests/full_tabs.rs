//! The calls a tabbed browser makes of webd (docs/lazywebplus-plan.md 2.3): `process_key`,
//! `detach_slots`, `recover_group` and `set_user_agent` of `FullWebView` against the service over the
//! fake web.

mod support;

use xui_webd_client::full::{Scope, ViewConfig};
use xui_webd_client::transport::errno::EACCES;
use xui_webd_client::Error;

use support::full::{browser_config, web};

fn tab(key: &str) -> ViewConfig {
    ViewConfig {
        process_key: key.into(),
        ..browser_config()
    }
}

#[test]
fn detach_slots_frees_the_slots_and_reattach_brings_them_back() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, tab("a.test"));
    view.create(64, 48).expect("create");
    let attached = view.attached();
    assert!(attached.iter().filter(|a| **a).count() >= 3, "{attached:?}");
    view.detach_slots().unwrap();
    assert!(view.attached().iter().all(|a| !a), "slots stayed");
    // Detaching twice is fine, and the view still answers calls.
    view.detach_slots().unwrap();
    view.reload(false).unwrap();
    view.reattach_slots().unwrap();
    assert_eq!(view.attached(), attached);
}

#[test]
fn recover_group_and_set_user_agent_reach_the_engine() {
    let web = web();
    let config = ViewConfig {
        user_agent_suffix: "Tabs/1".into(),
        ..tab("a.test")
    };
    let mut view = web.view(7, Scope::Browser, config);
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    view.set_user_agent("Desktop/2").unwrap();
    view.set_user_agent("").unwrap();
    view.recover_group().unwrap();
    assert_eq!(
        web.web.agents(),
        vec![(id, "Desktop/2".to_owned()), (id, String::new())]
    );
    assert_eq!(web.web.group_recovers(), vec![id]);
}

#[test]
fn a_user_agent_cannot_be_set_on_a_view_made_without_one() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, tab("a.test"));
    view.create(64, 48).expect("create");
    assert_eq!(
        view.set_user_agent("Desktop/2"),
        Err(Error::Refused(EACCES))
    );
    assert!(web.web.agents().is_empty());
    // A full agent is the browser scope's: the net scope is refused when the view is made.
    let net = ViewConfig {
        user_agent: "X/1".into(),
        ..support::full::net_config()
    };
    let mut other = web.view(8, Scope::Net, net);
    assert!(other.create(64, 48).is_err());
}
