//! The calls a tabbed browser makes of webd (docs/lazywebplus-plan.md 2.3): `process_key`,
//! `detach_slots`, `recover_group` and `set_user_agent` of `FullWebView` against the service over the
//! fake web.

mod support;

use xui_webd::engine::EngineEvents;
use xui_webd_client::full::{FullEvent, Scope, ViewConfig};
use xui_webd_client::transport::errno::EACCES;
use xui_webd_client::Error;

use support::full::{browser_config, web};
use support::web::long_frame;

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

#[test]
fn reattaching_slots_that_are_attached_changes_nothing() {
    // Why: attaching rebuilds the slot table; done over slots webd still holds, the client would drop the
    // memory webd keeps painting into.
    let web = web();
    let mut view = web.view(7, Scope::Browser, tab("a.test"));
    view.create(32, 24).expect("create");
    let attached = view.attached();
    view.reattach_slots().unwrap();
    view.reattach_slots().unwrap();
    assert_eq!(view.attached(), attached);
    let queue = web.world.borrow().queue.clone();
    queue.frame(view.view_id().unwrap(), &long_frame(32, 24, 3), 32, 24);
    let events = view.poll();
    assert!(events.iter().any(|e| matches!(e, FullEvent::Frame(_))));
    assert_eq!(view.picture().rgba(), &long_frame(32, 24, 3)[..]);
}

#[test]
fn frames_queued_before_a_detach_cannot_paint_the_slots_attached_after_it() {
    // Why: webd sent a frame naming slot 0 before it handled the detach; the client attaches a new,
    // zeroed slot 0 and would copy it over the picture when it read the old event.
    let web = web();
    let mut view = web.view(7, Scope::Browser, tab("a.test"));
    view.create(32, 24).expect("create");
    let id = view.view_id().unwrap();
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(32, 24, 0), 32, 24);
    view.poll();
    let shown = view.picture().rgba().to_vec();
    assert_eq!(shown, long_frame(32, 24, 0));
    // A newer frame and a page state reach the client's channel; nothing reads them yet.
    queue.frame(id, &long_frame(32, 24, 7), 32, 24);
    queue.state(id, "Titled", false, 1, "");
    web.world.borrow_mut().pump();
    // A backlog longer than one poll reads (256 events), so no bounded read-ahead could clear it.
    for n in 0..600 {
        queue.scroll(id, 0, n, false);
        web.world.borrow_mut().pump();
    }
    view.detach_slots().unwrap();
    view.reattach_slots().unwrap();
    let mut events = Vec::new();
    for _ in 0..8 {
        events.extend(view.poll());
    }
    assert_eq!(
        view.picture().rgba(),
        &shown[..],
        "an old frame painted a new slot"
    );
    assert!(
        !events.iter().any(|e| matches!(e, FullEvent::Frame(_))),
        "{events:?}"
    );
    // The other events of the backlog are not lost.
    assert!(
        events
            .iter()
            .any(|e| matches!(e, FullEvent::State(state) if state.title == "Titled")),
        "{events:?}"
    );
    // A frame painted after the reattach is applied.
    queue.frame(id, &long_frame(32, 24, 11), 32, 24);
    view.poll();
    assert_eq!(view.picture().rgba(), &long_frame(32, 24, 11)[..]);
}

#[test]
fn bands_painted_after_a_reattach_are_given_back_even_though_they_are_not_read() {
    // Why: until a whole frame is applied, bands cannot be told from stale ones and are not read; but a band
    // in a slot attached since the detach holds that slot at webd until it is released, and two lost
    // releases would stop bands for good.
    let web = web();
    let mut view = web.view(7, Scope::Browser, tab("a.test"));
    view.create(32, 24).expect("create");
    let id = view.view_id().unwrap();
    let queue = web.world.borrow().queue.clone();
    queue.frame(id, &long_frame(32, 24, 0), 32, 24);
    view.poll();
    view.detach_slots().unwrap();
    view.reattach_slots().unwrap();
    let bands = |view: &mut support::full::Full| {
        (0..4)
            .flat_map(|_| view.poll())
            .filter(|event| matches!(event, FullEvent::Band(_)))
            .count()
    };
    for round in 0..4 {
        let doc_y = 24 + 20 * round;
        queue.band(id, 0, doc_y, 32, 20, &long_frame(32, 20, doc_y));
        queue.band(id, 0, doc_y + 20, 32, 20, &long_frame(32, 20, doc_y + 20));
        assert_eq!(bands(&mut view), 0, "round {round}: a band was read early");
    }
    // The whole frame ends the wait, and both band slots are free again to carry rows.
    queue.frame(id, &long_frame(32, 24, 5), 32, 24);
    view.poll();
    queue.band(id, 0, 24, 32, 20, &long_frame(32, 20, 24));
    queue.band(id, 0, 44, 32, 20, &long_frame(32, 20, 44));
    assert_eq!(bands(&mut view), 2);
}
