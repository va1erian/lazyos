//! A view made with `notify_in_client` (P4): the app shows the page's notifications, webd hands it
//! only what the view's profile allowed and passes the app's answers to the page.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, FEv, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::{CLIENT_NOTIFICATIONS_PER_VIEW, EVENT_BYTES_MAX, NOTIFY_TEXT_BYTES};
use xui_webd::recording::Call as EngineCall;

const SITE: &str = "https://example.com";
const SHOWN: u32 = wire::NOTIFY_STATE_KIND_SHOWN;
const CLICKED: u32 = wire::NOTIFY_STATE_KIND_CLICKED;
const CLOSED: u32 = wire::NOTIFY_STATE_KIND_CLOSED;

fn in_client() -> wire::ViewConfig {
    wire::ViewConfig {
        notifications: true,
        notify_in_client: true,
        ..browser()
    }
}

fn show(view: u64, notification: u64, title: &str) -> FEv {
    FEv::NotifyShow(wire::NotifyShowArgs {
        view,
        notification,
        origin: SITE.into(),
        page_url: format!("{SITE}/page"),
        title: title.into(),
        body: "body".into(),
        tag: "t".into(),
    })
}

/// A view with the flag, `SITE` allowed, and the engine's record and the events read.
fn allowed(fx: &mut Fx, app: xui_webd::ident::Origin) -> (u64, u64) {
    let (view, events) = fx.view(app, Via::Browser, in_client(), 4, 4);
    fx.notify_permission(app, view, SITE, wire::PERMISSION_ANSWER_ALLOW)
        .unwrap();
    fx.engine.take();
    fx.news(events);
    (view, events)
}

#[test]
fn an_allowed_sites_notification_goes_to_the_app_and_never_to_webds_toasts() {
    // Why: the app posts the toast so that a click on it brings the app's window forward; webd must
    // not post one as well, and must hand over every text the app needs to title it itself.
    let mut fx = Fx::new("notify-client-show");
    let app = origin(7);
    let (view, events) = allowed(&mut fx, app);
    fx.queue
        .notify_show_from(view, 7, SITE, &format!("{SITE}/page"), "Hi", "body", "t");
    fx.pump(0);
    assert_eq!(fx.news(events), vec![show(view, 7, "Hi")]);
    assert!(fx.outside.toasts().is_empty(), "webd posted no toast");
    assert!(fx.engine.take().is_empty(), "the page waits for the app");
    // The app's answers reach the page in turn.
    fx.notify_state(app, view, 7, SHOWN).unwrap();
    fx.notify_state(app, view, 7, CLICKED).unwrap();
    fx.notify_state(app, view, 7, CLOSED).unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            EngineCall::NotifyShown(7),
            EngineCall::NotifyClicked(7),
            EngineCall::NotifyClosed(7)
        ]
    );
    // Closed is the end: the id is not the app's to speak about any more.
    assert_eq!(fx.notify_state(app, view, 7, CLICKED), Err(ESRCH));
}

#[test]
fn a_site_the_profile_did_not_allow_reaches_no_app() {
    // Why: the profile's decision is the gate, whoever shows the toast.
    let mut fx = Fx::new("notify-client-undecided");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, in_client(), 4, 4);
    fx.engine.take();
    fx.queue.notify_show(view, 7, SITE, "Hi", "");
    fx.pump(0);
    assert!(fx.news(events).is_empty());
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(7)]);
    // An id the app was never given is refused.
    assert_eq!(fx.notify_state(app, view, 7, SHOWN), Err(ESRCH));
}

#[test]
fn the_pages_close_and_the_end_of_its_object_are_forwarded() {
    // Why: the app takes its toast down when the page closes it, and keeps it (but runs no script)
    // when the page's object is gone.
    let mut fx = Fx::new("notify-client-end");
    let app = origin(7);
    let (view, events) = allowed(&mut fx, app);
    fx.queue.notify_show(view, 7, SITE, "A", "");
    fx.queue.notify_show(view, 8, SITE, "B", "");
    fx.pump(0);
    fx.news(events);
    fx.queue.notify_cancel(view, 7);
    fx.queue.notify_gone(view, 8);
    // An id webd never handed out is nobody's news.
    fx.queue.notify_cancel(view, 99);
    fx.pump(1);
    assert_eq!(
        fx.news(events),
        vec![
            FEv::NotifyCancel(wire::NotifyCancelArgs {
                view,
                notification: 7
            }),
            FEv::NotifyGone(wire::NotifyGoneArgs {
                view,
                notification: 8
            }),
        ]
    );
    assert_eq!(fx.notify_state(app, view, 7, CLOSED), Err(ESRCH));
    // A gone page has no script: the click and the close only end the entry.
    fx.notify_state(app, view, 8, CLICKED).unwrap();
    fx.notify_state(app, view, 8, CLOSED).unwrap();
    assert!(fx.engine.take().is_empty());
    assert_eq!(fx.notify_state(app, view, 8, CLOSED), Err(ESRCH));
}

#[test]
fn notify_state_is_refused_for_another_view_a_view_without_the_flag_and_garbage() {
    // Why: an app speaks only about the notifications of its own view that webd handed it.
    let mut fx = Fx::new("notify-client-refused");
    let app = origin(7);
    let other = origin(8);
    let (view, _) = allowed(&mut fx, app);
    fx.queue.notify_show(view, 7, SITE, "A", "");
    fx.pump(0);
    assert_eq!(fx.notify_state(other, view, 7, SHOWN), Err(ENOENT));
    assert_eq!(fx.notify_state(app, view + 1000, 7, SHOWN), Err(ENOENT));
    assert_eq!(fx.notify_state(app, view, 7, 9), Err(EINVAL));
    let plain = wire::ViewConfig {
        notifications: true,
        ..browser()
    };
    let (toasting, _) = fx.view(app, Via::Browser, plain, 4, 4);
    assert_eq!(fx.notify_state(app, toasting, 7, SHOWN), Err(EACCES));
    // The flag without notifications does nothing: the view has no notifications at all.
    let flag_only = wire::ViewConfig {
        notify_in_client: true,
        ..browser()
    };
    let (quiet, _) = fx.view(app, Via::Browser, flag_only, 4, 4);
    assert_eq!(fx.notify_state(app, quiet, 7, SHOWN), Err(EACCES));
    assert!(fx.engine.take().iter().all(|call| !matches!(
        call,
        EngineCall::NotifyShown(_) | EngineCall::NotifyClicked(_)
    )));
}

#[test]
fn a_page_cannot_open_more_than_the_bound_and_is_told_the_rest_closed() {
    // Why: a page in a loop of `new Notification` must not grow webd's table or the app's.
    let mut fx = Fx::new("notify-client-bound");
    let app = origin(7);
    let (view, events) = allowed(&mut fx, app);
    let count = CLIENT_NOTIFICATIONS_PER_VIEW as u64;
    for id in 1..=count + 3 {
        fx.queue.notify_show(view, id, SITE, "x", "");
    }
    fx.pump(0);
    let shown = fx
        .news(events)
        .into_iter()
        .filter(|e| matches!(e, FEv::NotifyShow(_)))
        .count();
    assert_eq!(shown, CLIENT_NOTIFICATIONS_PER_VIEW);
    assert_eq!(
        fx.engine.take(),
        (count + 1..=count + 3)
            .map(EngineCall::NotifyClosed)
            .collect::<Vec<_>>()
    );
    // One closed by the app makes room again.
    fx.notify_state(app, view, 1, CLOSED).unwrap();
    fx.queue.notify_show(view, 500, SITE, "x", "");
    fx.pump(1);
    assert_eq!(fx.news(events).len(), 1);
}

#[test]
fn a_notification_with_the_longest_texts_fits_the_clients_buffer() {
    // Why: the kernel drops an event larger than the client's receive buffer; five page texts at
    // their largest must still arrive, cut.
    let mut fx = Fx::new("notify-client-big");
    let app = origin(7);
    let (view, events) = allowed(&mut fx, app);
    let huge = "\u{20ac}".repeat(64 << 10);
    fx.queue
        .notify_show_from(view, 7, SITE, &huge, &huge, &huge, &huge);
    fx.pump(0);
    let sent = fx.rig.fabric.take_tagged(events);
    let (_, method, body) = sent
        .iter()
        .find(|(_, method, _)| *method == wire::METHOD_NOTIFYSHOW)
        .expect("the notification went out");
    assert_eq!(*method, wire::METHOD_NOTIFYSHOW);
    assert!(body.len() + 256 <= EVENT_BYTES_MAX, "{} bytes", body.len());
    let args = wire::decode_notify_show_args(body).unwrap();
    assert!(args.title.len() <= NOTIFY_TEXT_BYTES && !args.title.is_empty());
    assert!(args.tag.len() <= NOTIFY_TEXT_BYTES);
}
