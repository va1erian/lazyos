//! A page's notifications: which sites may show them, the toasts webd posts under its own name,
//! and what a click does. Uses a recording `Outside` in place of the shell's notification service.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, FEv, Fx, Via};
use support::origin;
use xui_webd::engine::{EngineEvents, LOST_CRASHED};
use xui_webd::fabric::errno::*;
use xui_webd::limits::{CLIENT_TOAST_REFILL_MS, TOASTS_PER_VIEW};
use xui_webd::outside::ToastNews;
use xui_webd::recording::Call as EngineCall;
use xui_webd::Note;

const SITE: &str = "https://example.com";
const ALLOW: u32 = wire::PERMISSION_ANSWER_ALLOW;
const BLOCK: u32 = wire::PERMISSION_ANSWER_BLOCK;
const DISMISS: u32 = wire::PERMISSION_ANSWER_DISMISS;

fn notifying() -> wire::ViewConfig {
    wire::ViewConfig {
        notifications: true,
        ..browser()
    }
}

fn permission(view: u64, origin: &str, answer: i32) -> EngineCall {
    EngineCall::NotifyPermission {
        view,
        origin: origin.into(),
        answer,
    }
}

#[test]
fn a_site_nobody_allowed_gets_no_toast_and_the_page_is_told_it_closed() {
    // Why: the page's `new Notification` waits for the engine; a notification from a site the
    // person did not allow must never reach the shell, and the page must not hang.
    let mut fx = Fx::new("notify-undecided");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.engine.take();
    fx.queue.notify_show(view, 7, SITE, "Hello", "body");
    fx.pump(0);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(7)]);
    assert!(fx.outside.toasts().is_empty());
    assert!(fx.news(events).is_empty());
    // A site that was blocked is no different.
    fx.notify_permission(app, view, SITE, BLOCK).unwrap();
    fx.engine.take();
    fx.queue.notify_show(view, 8, SITE, "Hello", "body");
    fx.pump(1);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(8)]);
    assert!(fx.outside.toasts().is_empty());
}

#[test]
fn an_allowed_sites_toast_is_titled_with_its_host_never_with_its_own_words() {
    // Why: the toast is webd's, shown by the shell under webd's name; a page that could title it
    // could pass as the system or another site.
    let mut fx = Fx::new("notify-toast");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.notify_permission(app, view, "https://shop.example.net:8443", ALLOW)
        .unwrap();
    assert_eq!(
        fx.engine.take().into_iter().skip(1).collect::<Vec<_>>(),
        vec![
            permission(view, SITE, 1),
            permission(view, "https://shop.example.net:8443", 1)
        ]
    );
    fx.queue
        .notify_show(view, 7, SITE, "System: your password expired", "click here");
    fx.queue
        .notify_show(view, 8, "https://shop.example.net:8443", "Order", "");
    fx.pump(0);
    assert_eq!(
        fx.outside.toasts(),
        vec![
            (
                101,
                "example.com".to_string(),
                "System: your password expired\nclick here".to_string()
            ),
            (
                102,
                "shop.example.net:8443".to_string(),
                "Order".to_string()
            ),
        ]
    );
    assert_eq!(
        fx.engine.take(),
        vec![EngineCall::NotifyShown(7), EngineCall::NotifyShown(8)]
    );
    // The decision is for the origin exactly as the engine names it.
    fx.queue
        .notify_show(view, 9, "http://example.com", "Hello", "");
    fx.pump(1);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(9)]);
}

#[test]
fn a_click_runs_the_pages_handler_and_tells_the_client_once() {
    // Why: the click is the only way back from a toast to the page; the client may raise its
    // window on it, and the engine runs the page's `onclick`.
    let mut fx = Fx::new("notify-click");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.queue.notify_show(view, 7, SITE, "Hello", "");
    fx.queue.notify_show(view, 8, SITE, "Second", "");
    fx.pump(0);
    fx.engine.take();
    fx.outside.news(ToastNews::Clicked { toast: 101 });
    fx.pump(1);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClicked(7)]);
    assert_eq!(
        fx.news(events),
        vec![FEv::NotifyClicked(wire::NotifyClickedArgs {
            view,
            notification: 7
        })]
    );
    // The toast is spent: a second click on it (a stale report) does nothing.
    fx.outside.news(ToastNews::Clicked { toast: 101 });
    // A toast the shell took down is the page's `close`.
    fx.outside.news(ToastNews::Closed { toast: 102 });
    // A toast webd never made is nobody's.
    fx.outside.news(ToastNews::Clicked { toast: 9999 });
    fx.pump(2);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(8)]);
    assert!(fx.news(events).is_empty());
}

#[test]
fn the_page_taking_a_notification_back_takes_its_toast_down_and_a_gone_page_object_keeps_it() {
    // Why: `notification.close()` must remove the toast; when the page's object is gone (a
    // navigation), the toast stays up but a click has no handler to run.
    let mut fx = Fx::new("notify-cancel");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.queue.notify_show(view, 7, SITE, "A", "");
    fx.queue.notify_show(view, 8, SITE, "B", "");
    fx.pump(0);
    fx.engine.take();
    fx.queue.notify_cancel(view, 7);
    fx.queue.notify_gone(view, 8);
    fx.pump(1);
    assert_eq!(fx.outside.closed(), vec![101]);
    fx.outside.news(ToastNews::Clicked { toast: 102 });
    fx.pump(2);
    assert!(
        fx.engine.take().is_empty(),
        "the page's object is gone: nothing to run"
    );
    assert_eq!(
        fx.news(events).len(),
        1,
        "the client still hears of the click"
    );
}

#[test]
fn a_view_shows_four_toasts_at_once_and_a_shell_that_refuses_is_a_close() {
    // Why: a page that notifies in a loop must not fill the screen; the shell may also refuse
    // (muted, do-not-disturb), and the page is then told its notification closed.
    let mut fx = Fx::new("notify-limit");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    // Different sites, a pace apart: only the view's own count is in the way.
    let sites: Vec<String> = (1..=7).map(|n| format!("https://s{n}.test")).collect();
    for site in &sites {
        fx.notify_permission(app, view, site, ALLOW).unwrap();
    }
    fx.engine.take();
    let step = CLIENT_TOAST_REFILL_MS;
    for n in 0..=TOASTS_PER_VIEW as u64 {
        fx.queue
            .notify_show(view, n + 1, &sites[n as usize], "n", "");
        fx.pump(n * step);
    }
    assert_eq!(fx.outside.toasts().len(), TOASTS_PER_VIEW);
    let last = EngineCall::NotifyClosed(TOASTS_PER_VIEW as u64 + 1);
    assert_eq!(fx.engine.take().last(), Some(&last));
    // A toast the person dismissed makes room.
    let mut now = 5 * step;
    fx.outside.news(ToastNews::Closed { toast: 101 });
    fx.pump(now);
    fx.engine.take();
    now += step;
    fx.queue.notify_show(view, 50, &sites[5], "n", "");
    fx.pump(now);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyShown(50)]);
    fx.outside.refuse_toasts(true);
    fx.outside.news(ToastNews::Closed { toast: 102 });
    now += step;
    fx.pump(now);
    fx.engine.take();
    now += step;
    fx.queue.notify_show(view, 51, &sites[6], "n", "");
    fx.pump(now);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(51)]);
    assert!(fx.service.take_notes().contains(&Note::Full {
        view,
        what: "toast refused"
    }));
}

#[test]
fn forgetting_a_site_makes_its_next_request_ask_the_person_again() {
    // Why: the person can revoke a decision (LazyWeb's site settings); webd's per-profile record
    // must go with the engine's, or a page would still be answered from the old decision.
    let mut fx = Fx::new("notify-forget");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.notify_permission(app, view, "https://kept.test", BLOCK)
        .unwrap();
    fx.engine.take();
    fx.queue.notify_permission_request(view, SITE);
    fx.pump(0);
    assert_eq!(fx.engine.take(), vec![permission(view, SITE, 1)]);
    assert!(fx.news(events).is_empty());

    // The spelling of the origin does not matter: the key is the normalised origin.
    let spelled = "HTTPS://Example.com:443";
    fx.notify_forget(app, view, spelled).unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![EngineCall::NotifyForget(view, spelled.into())]
    );
    fx.queue.notify_permission_request(view, SITE);
    fx.queue
        .notify_permission_request(view, "https://kept.test");
    fx.pump(1);
    assert_eq!(
        fx.engine.take(),
        vec![permission(view, "https://kept.test", 0)],
        "only the forgotten site is not answered by webd"
    );
    assert_eq!(
        fx.news(events),
        vec![FEv::NotifyAsk(wire::NotifyPermissionRequestArgs {
            view,
            origin: SITE.into()
        })]
    );
    // And it no longer gets a toast.
    fx.queue.notify_show(view, 9, SITE, "Hello", "body");
    fx.pump(2);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(9)]);
    // Forgetting a site nobody decided about is not an error.
    fx.notify_forget(app, view, "https://never.test").unwrap();
}

#[test]
fn a_page_process_starts_with_the_decisions_every_profile_kept() {
    // Why: a client tells webd its kept decisions when it makes a view (LazyWeb's notify.tsv, which
    // webd forgets when it stops); a page process that starts later must already read them as
    // `Notification.permission`, which WebKit takes from the provider at the process's start.
    let mut fx = Fx::new("notify-start");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    assert_eq!(fx.queue.notify_decision_at(0), None);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.notify_permission(app, view, "https://kept.test", BLOCK)
        .unwrap();
    // A dismissal is no decision.
    fx.notify_permission(
        app,
        view,
        "https://later.test",
        wire::PERMISSION_ANSWER_DISMISS,
    )
    .unwrap();
    assert_eq!(
        fx.queue.notify_decision_at(0),
        Some(("https://example.com".into(), true))
    );
    assert_eq!(
        fx.queue.notify_decision_at(1),
        Some(("https://kept.test".into(), false))
    );
    assert_eq!(fx.queue.notify_decision_at(2), None);
    fx.notify_forget(app, view, SITE).unwrap();
    assert_eq!(
        fx.queue.notify_decision_at(0),
        Some(("https://kept.test".into(), false))
    );
    assert_eq!(fx.queue.notify_decision_at(1), None);
}

#[test]
fn forgetting_needs_the_notifications_feature_a_good_origin_and_the_callers_own_view() {
    let mut fx = Fx::new("notify-forget-refused");
    let (app, other) = (origin(7), origin(8));
    let (plain, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.engine.take();
    assert_eq!(fx.notify_forget(app, plain, SITE), Err(EACCES));
    assert_eq!(fx.notify_forget(other, view, SITE), Err(ENOENT));
    assert_eq!(fx.notify_forget(app, view + 1000, SITE), Err(ENOENT));
    let long = format!("https://{}.test", "a".repeat(300));
    for bad in ["", "https://a\0b.test", long.as_str()] {
        let result = fx.notify_forget(app, view, bad);
        assert!(result.is_err(), "{bad:?}");
    }
    assert!(
        fx.engine.take().is_empty(),
        "a refused call reached the engine"
    );
    fx.queue.view_lost(view, LOST_CRASHED);
    fx.pump(0);
    assert_eq!(fx.notify_forget(app, view, SITE), Err(ENODEV));
    assert!(fx.engine.take().is_empty());
}

#[test]
fn a_decided_site_is_answered_at_once_and_an_undecided_one_asks_the_client() {
    // Why: the person is asked once per site; later requests in any view of the profile are
    // answered by webd, and a dismissed bar is not a decision.
    let mut fx = Fx::new("notify-ask");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.engine.take();
    fx.queue.notify_permission_request(view, SITE);
    fx.pump(0);
    assert_eq!(
        fx.news(events),
        vec![FEv::NotifyAsk(wire::NotifyPermissionRequestArgs {
            view,
            origin: SITE.into()
        })]
    );
    assert!(fx.engine.take().is_empty(), "the page waits for the person");

    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.notify_permission(app, view, "https://blocked.test", BLOCK)
        .unwrap();
    fx.notify_permission(app, view, "https://later.test", DISMISS)
        .unwrap();
    assert_eq!(
        fx.engine.take(),
        vec![
            permission(view, SITE, 1),
            permission(view, "https://blocked.test", 0),
            permission(view, "https://later.test", -1),
        ]
    );
    for site in [SITE, "https://blocked.test"] {
        fx.queue.notify_permission_request(view, site);
    }
    fx.queue
        .notify_permission_request(view, "https://later.test");
    fx.pump(1);
    assert_eq!(
        fx.engine.take(),
        vec![
            permission(view, SITE, 1),
            permission(view, "https://blocked.test", 0)
        ],
        "answered by webd, with no event"
    );
    assert_eq!(
        fx.news(events).len(),
        1,
        "only the dismissed site asks again"
    );

    // The decision belongs to the profile: another client's view of it is answered the same.
    let (other, other_events) = fx.view(origin(8), Via::Browser, notifying(), 4, 4);
    fx.engine.take();
    fx.queue.notify_permission_request(other, SITE);
    fx.pump(2);
    assert_eq!(fx.engine.take(), vec![permission(other, SITE, 1)]);
    assert!(fx.news(other_events).is_empty());
}

#[test]
fn a_permission_answer_needs_a_view_made_with_notifications_and_a_sane_origin() {
    // Why: the answer is kept for the profile and handed to the engine as a C string.
    let mut fx = Fx::new("notify-refused");
    let app = origin(7);
    let (quiet, _) = fx.view(app, Via::Browser, browser(), 4, 4);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.engine.take();
    assert_eq!(fx.notify_permission(app, quiet, SITE, ALLOW), Err(EACCES));
    assert_eq!(fx.notify_permission(app, view, SITE, 7), Err(EINVAL));
    assert_eq!(fx.notify_permission(app, view, "", ALLOW), Err(EINVAL));
    assert_eq!(
        fx.notify_permission(app, view, "https://a\0b", ALLOW),
        Err(EINVAL)
    );
    let long = format!("https://{}", "a".repeat(260));
    assert_eq!(fx.notify_permission(app, view, &long, ALLOW), Err(EINVAL));
    assert_eq!(
        fx.notify_permission(origin(8), view, SITE, ALLOW),
        Err(ENOENT)
    );
    assert!(fx.engine.take().is_empty());
    // Nothing was decided by the refused calls: the site is still undecided.
    fx.queue.notify_show(view, 1, SITE, "n", "");
    fx.pump(0);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(1)]);
}

#[test]
fn a_toast_outlives_its_view_but_a_click_on_it_reaches_no_page() {
    // Why: the person may click a toast after the page's window closed; there is no page to run
    // a handler in, and no client to tell, and nothing may panic or be sent to a stranger.
    let mut fx = Fx::new("notify-orphan");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.queue.notify_show(view, 7, SITE, "n", "");
    fx.pump(0);
    fx.destroy_view(app, view).unwrap();
    fx.engine.take();
    fx.outside.news(ToastNews::Clicked { toast: 101 });
    fx.pump(1);
    assert!(fx.engine.take().is_empty());
    assert!(fx.news(events).is_empty());
    assert_eq!(fx.fabric.double_closes(), 0);
}
