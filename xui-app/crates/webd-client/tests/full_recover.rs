//! `FullWebView::recover` and `notify_forget` (LazyWeb, P4) against the service over the fake web:
//! what reaches the engine, and what the service refuses.

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::engine::{EngineEvents, LOST_OTHER};
use xui_webd_client::full::{FullEvent, Scope, ViewConfig};
use xui_webd_client::transport::errno::EACCES;
use xui_webd_client::{Error, Lost};

use support::full::{browser_config, web, Full};

fn drain(view: &mut Full) -> Vec<FullEvent> {
    let mut all = Vec::new();
    for _ in 0..4 {
        all.extend(view.poll());
    }
    all
}

fn notifying() -> ViewConfig {
    ViewConfig {
        notifications: true,
        ..browser_config()
    }
}

#[test]
fn recover_reaches_the_engine_with_its_flag_and_a_lost_view_ends_the_page() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, browser_config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    view.recover(true).unwrap();
    view.recover(false).unwrap();
    assert_eq!(web.web.recovers(), vec![(id, true), (id, false)]);
    // Without a reload the engine reports the page as ended by request: the view is lost, and the
    // client makes a new one.
    let queue = web.world.borrow().queue.clone();
    queue.view_lost(id, LOST_OTHER);
    let events = drain(&mut view);
    let lost = Lost::Engine(wire::LOST_REASON_OTHER);
    assert_eq!(events, vec![FullEvent::Lost(lost)]);
    assert_eq!(view.recover(true), Err(Error::Lost(lost)));
    assert_eq!(
        web.web.recovers().len(),
        2,
        "a lost view reached the engine"
    );
}

#[test]
fn notify_forget_reaches_the_engine_and_asks_the_person_about_the_site_again() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, notifying());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    let site = "https://example.com";
    let allow = wire::PERMISSION_ANSWER_ALLOW;
    view.notify_permission(site, allow).unwrap();
    let queue = web.world.borrow().queue.clone();
    // A decided site is answered by webd: the client hears nothing.
    queue.notify_permission_request(id, site);
    let events = drain(&mut view);
    assert!(!events
        .iter()
        .any(|e| matches!(e, FullEvent::NotifyPermissionRequest { .. })));
    assert_eq!(web.web.permissions().last(), Some(&(id, site.into(), 1)));

    view.notify_forget(site).unwrap();
    assert_eq!(web.web.forgets(), vec![(id, site.to_owned())]);
    queue.notify_permission_request(id, site);
    let events = drain(&mut view);
    let asked = FullEvent::NotifyPermissionRequest {
        origin: site.to_owned(),
    };
    assert!(events.contains(&asked), "{events:?}");
}

#[test]
fn notify_forget_is_refused_for_a_view_made_without_notifications() {
    let web = web();
    let mut view = web.view(7, Scope::Browser, browser_config());
    view.create(64, 48).expect("create");
    assert_eq!(
        view.notify_forget("https://example.com"),
        Err(Error::Refused(EACCES))
    );
    assert!(web.web.forgets().is_empty());
    assert!(view.is_live(), "a refusal does not end the view");
}
