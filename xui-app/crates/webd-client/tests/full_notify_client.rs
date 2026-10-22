//! A view made with `notify_in_client` (LazyWeb+, P4) against the service and the recording
//! engine: the page's notification reaches the app as `FullEvent::NotifyShow`, its close and the end
//! of its object follow, and `notify_state` reaches the engine.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use xui_webd::events::EventQueue;
use xui_webd::fullconfig::Places;
use xui_webd::recording::{Call, RecordingEngine};
use xui_webd_client::full::{FullEvent, FullWebView, Scope, ViewConfig};
use xui_webd_client::transport::errno::ESRCH;
use xui_webd_client::Error;

use support::{world_with, SharedWorld, TestTransport};

const SITE: &str = "https://example.com";

fn world() -> (SharedWorld, RecordingEngine, Arc<EventQueue>) {
    let engine = RecordingEngine::new();
    let queue = Arc::new(EventQueue::new());
    let places = Places {
        profiles: Some(PathBuf::from("/webd-test/profiles")),
        downloads: Some(std::env::temp_dir().join("webd-client-notify-client")),
    };
    (
        world_with(engine.clone(), queue.clone(), places),
        engine,
        queue,
    )
}

fn config() -> ViewConfig {
    ViewConfig {
        profile: "browser".into(),
        scheme_policy: wire::SCHEME_POLICY_WEB,
        notifications: true,
        notify_in_client: true,
        ..ViewConfig::default()
    }
}

fn drain(view: &mut FullWebView<TestTransport>) -> Vec<FullEvent> {
    let mut all = Vec::new();
    for _ in 0..4 {
        all.extend(view.poll());
    }
    all.retain(|event| !matches!(event, FullEvent::State(_) | FullEvent::Frame(_)));
    all
}

fn shown(event: &FullEvent) -> Option<u64> {
    match event {
        FullEvent::NotifyShow { notification, .. } => Some(*notification),
        _ => None,
    }
}

#[test]
fn the_app_gets_an_allowed_sites_notification_and_its_answers_reach_the_page() {
    let (world, engine, queue) = world();
    let mut view = FullWebView::new(TestTransport::full(&world, 7), Scope::Browser, config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    view.notify_permission(SITE, wire::PERMISSION_ANSWER_ALLOW)
        .unwrap();
    drain(&mut view);
    queue.notify_show_from(id, 5, SITE, "https://example.com/p", "Hi", "There", "tag");
    let events = drain(&mut view);
    assert_eq!(
        events,
        vec![FullEvent::NotifyShow {
            notification: 5,
            origin: SITE.into(),
            page_url: "https://example.com/p".into(),
            title: "Hi".into(),
            body: "There".into(),
            tag: "tag".into(),
        }]
    );
    engine.take();
    view.notify_state(5, wire::NOTIFY_STATE_KIND_SHOWN).unwrap();
    view.notify_state(5, wire::NOTIFY_STATE_KIND_CLICKED)
        .unwrap();
    assert_eq!(
        engine.take(),
        vec![Call::NotifyShown(5), Call::NotifyClicked(5)]
    );
    // The page closes it: the app hears it and the id is over.
    queue.notify_cancel(id, 5);
    assert_eq!(
        drain(&mut view),
        vec![FullEvent::NotifyCancel { notification: 5 }]
    );
    assert_eq!(
        view.notify_state(5, wire::NOTIFY_STATE_KIND_CLOSED),
        Err(Error::Refused(ESRCH))
    );
    assert!(view.is_live(), "a refused answer leaves the view");
}

#[test]
fn a_site_without_a_decision_never_reaches_the_app_and_a_gone_page_is_reported() {
    let (world, engine, queue) = world();
    let mut view = FullWebView::new(TestTransport::full(&world, 7), Scope::Browser, config());
    view.create(64, 48).expect("create");
    let id = view.view_id().unwrap();
    queue.notify_show(id, 1, SITE, "Hi", "");
    assert!(drain(&mut view).iter().all(|e| shown(e).is_none()));
    assert!(engine.take().contains(&Call::NotifyClosed(1)));
    view.notify_permission(SITE, wire::PERMISSION_ANSWER_ALLOW)
        .unwrap();
    queue.notify_show(id, 2, SITE, "Hi", "");
    queue.notify_gone(id, 2);
    let events = drain(&mut view);
    assert_eq!(events.iter().filter_map(shown).collect::<Vec<_>>(), vec![2]);
    assert!(events.contains(&FullEvent::NotifyGone { notification: 2 }));
}
