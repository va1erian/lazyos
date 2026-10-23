//! What keeps a page's notifications from spoofing or starving (review of 2026-10-22, finding 3): the
//! toast's title is the site with a long host cut at its START, the page's words are cleaned, a site has
//! a few live toasts and a pace, and each client has a share of the shell's allowance for webd (the one
//! sender of every client's toasts), so one page looping `new Notification(); close()` cannot use up what
//! another client needs. Uses a recording `Outside` in place of the shell's notification service.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{browser, Fx, Via};
use support::origin;
use xui_webd::limits::{CLIENT_TOAST_BURST, CLIENT_TOAST_REFILL_MS, TOASTS_PER_CLIENT};
use xui_webd::recording::Call as EngineCall;
use xui_webd::Note;

const ALLOW: u32 = wire::PERMISSION_ANSWER_ALLOW;
const SITE: &str = "https://example.com";

fn notifying() -> wire::ViewConfig {
    wire::ViewConfig {
        notifications: true,
        ..browser()
    }
}

fn noted(fx: &mut Fx, view: u64, what: &'static str) -> bool {
    fx.service.take_notes().contains(&Note::Full { view, what })
}

#[test]
fn a_long_host_keeps_its_end_so_a_spoof_cannot_fill_the_title() {
    // Why: the shell cuts a title to 64 characters keeping its START; a page on
    // `accounts.bank.example.com.secure-<padding>.attacker.test` would show the bank's name. The title
    // is cut at its beginning instead, where the registrable part is.
    let mut fx = Fx::new("notify-spoof");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    let long = format!(
        "https://accounts.bank.example.com.secure-{}.attacker.test",
        "x".repeat(60)
    );
    fx.notify_permission(app, view, &long, ALLOW).unwrap();
    fx.queue.notify_show(view, 1, &long, "Sign in", "now");
    fx.pump(0);
    let toasts = fx.outside.toasts();
    assert_eq!(toasts.len(), 1);
    let title = &toasts[0].1;
    assert!(title.chars().count() <= 64, "{title}");
    assert!(
        title.starts_with("...") && title.ends_with(".attacker.test"),
        "{title}"
    );
    assert!(!title.contains("accounts.bank"), "{title}");
}

#[test]
fn the_pages_words_are_cleaned_and_only_a_web_page_has_a_toast() {
    // Why: control and bidirectional-override characters in a page's words could reorder or hide
    // text in the shell; `data:` and `blob:` pages have no site a person could have allowed.
    let mut fx = Fx::new("notify-clean");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.queue
        .notify_show(view, 1, SITE, "pay\u{202E}now\u{7}", "a\r\nb\u{200B}c");
    fx.pump(0);
    assert_eq!(
        fx.outside.toasts(),
        vec![(101, "example.com".to_string(), "paynow\na bc".to_string())]
    );
    let blob = "blob:https://example.com/0e5a-uuid";
    fx.notify_permission(app, view, blob, ALLOW).unwrap();
    fx.engine.take();
    fx.queue.notify_show(view, 2, blob, "x", "");
    fx.pump(CLIENT_TOAST_REFILL_MS);
    assert_eq!(fx.engine.take(), vec![EngineCall::NotifyClosed(2)]);
    assert_eq!(fx.outside.toasts().len(), 1, "no toast for it");
}

#[test]
fn a_site_has_three_live_toasts() {
    // Why: one site must not fill the screen even slowly; the cap is LazyWeb's.
    let mut fx = Fx::new("notify-site-limit");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(app, view, SITE, ALLOW).unwrap();
    fx.engine.take();
    for n in 0..4u64 {
        fx.queue.notify_show(view, n + 1, SITE, "n", "");
        fx.pump(n * CLIENT_TOAST_REFILL_MS);
    }
    assert_eq!(fx.outside.toasts().len(), 3);
    assert_eq!(fx.engine.take().last(), Some(&EngineCall::NotifyClosed(4)));
    assert!(noted(&mut fx, view, "toast limit: site"));
}

#[test]
fn a_client_keeps_five_toasts_over_all_its_views() {
    // Why: webd is the shell's one sender for every client; the shell keeps 16 toasts for it, so a
    // client gets a share of them across its views, not a share per view.
    let mut fx = Fx::new("notify-client-limit");
    let app = origin(7);
    let (first, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    let (second, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    let sites: Vec<String> = (1..=8).map(|n| format!("https://s{n}.test")).collect();
    for site in &sites {
        fx.notify_permission(app, first, site, ALLOW).unwrap();
    }
    fx.engine.take();
    let views = [first, first, first, first, second, second];
    for (n, view) in views.iter().enumerate() {
        fx.queue
            .notify_show(*view, n as u64 + 1, &sites[n], "n", "");
        fx.pump(n as u64 * CLIENT_TOAST_REFILL_MS);
    }
    assert_eq!(fx.outside.toasts().len(), TOASTS_PER_CLIENT);
    assert_eq!(fx.engine.take().last(), Some(&EngineCall::NotifyClosed(6)));
    assert!(noted(&mut fx, second, "toast limit: client"));
}

#[test]
fn one_clients_notification_loop_leaves_another_client_its_share() {
    // Why: the shell's sender-wide bucket (a burst of 6, one more per half second) is webd's for all
    // clients. A page that loops `new Notification(); close()` must be held to its client's pace, or
    // every other client's notifications are refused for as long as it runs.
    let mut fx = Fx::new("notify-flood");
    let (alice, bob) = (origin(7), origin(8));
    let (flood, _) = fx.view(alice, Via::Browser, notifying(), 4, 4);
    let (calm, _) = fx.view(bob, Via::Browser, notifying(), 4, 4);
    fx.notify_permission(alice, flood, SITE, ALLOW).unwrap();
    fx.notify_permission(bob, calm, "https://news.test", ALLOW)
        .unwrap();
    fx.engine.take();
    // Two seconds of a tight loop: show, then close, 200 times.
    for n in 0..200u64 {
        fx.queue.notify_show(flood, n + 1, SITE, "spam", "");
        fx.pump(n * 10);
        fx.queue.notify_cancel(flood, n + 1);
        fx.pump(n * 10 + 5);
    }
    let posted = fx.outside.toasts().len() as u64;
    assert!(
        posted <= CLIENT_TOAST_BURST + 1,
        "{posted} toasts reached the shell from one client's loop"
    );
    // The other client still gets its toast.
    fx.queue
        .notify_show(calm, 1000, "https://news.test", "News", "");
    fx.pump(2100);
    let toasts = fx.outside.toasts();
    assert_eq!(toasts.len() as u64, posted + 1);
    assert_eq!(toasts.last().map(|t| t.1.as_str()), Some("news.test"));
    assert!(fx.engine.take().contains(&EngineCall::NotifyShown(1000)));
}

#[test]
fn a_close_the_shell_never_reported_does_not_hold_the_allowance_for_good() {
    // Why: a lost report would otherwise keep a toast counted for the client's whole session.
    let mut fx = Fx::new("notify-expire");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Browser, notifying(), 4, 4);
    let sites: Vec<String> = (1..=6).map(|n| format!("https://s{n}.test")).collect();
    for site in &sites {
        fx.notify_permission(app, view, site, ALLOW).unwrap();
    }
    for n in 0..4u64 {
        fx.queue
            .notify_show(view, n + 1, &sites[n as usize], "n", "");
        fx.pump(n * CLIENT_TOAST_REFILL_MS);
    }
    assert_eq!(fx.outside.toasts().len(), 4);
    // Over the view's count while all four are held...
    fx.queue.notify_show(view, 5, &sites[4], "n", "");
    fx.pump(10_000);
    assert_eq!(fx.outside.toasts().len(), 4);
    // ...and room again once the shell's silence has lasted longer than a toast lives.
    fx.pump(100_000);
    fx.queue.notify_show(view, 6, &sites[5], "n", "");
    fx.pump(100_001);
    assert_eq!(fx.outside.toasts().len(), 5);
}
