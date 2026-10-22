//! What a full view's page asks the app, and what the service decides without it: held navigations
//! (`Handoff.Ask`) and hand-offs to another program (`Handoff.Browser`, which needs the user's own
//! input). Dialogs are in `full_dialogs.rs`, the page's small news in `full_events.rs`.

#![cfg(not(feature = "host"))]

mod support;

use messenger_generated::os_lazy_webd_view_v1 as wire;
use support::full::{listed, web, FEv, Fx, Via};
use support::origin;
use xui_webd::fabric::errno::*;
use xui_webd::limits::{HELD_PER_VIEW, HOLD_MS, INPUT_LOCK_MS};
use xui_webd::policy::Verdict;
use xui_webd::recording::Call as EngineCall;
use xui_webd::Note;

const LINK: i32 = wire::NAVIGATION_KIND_LINK_CLICKED as i32;

fn asking() -> wire::ViewConfig {
    wire::ViewConfig {
        handoff: wire::HANDOFF_ASK,
        ..listed(&["http://10.0.2.2:18081"])
    }
}

fn answer(view: u64, request: u64, allow: bool) -> EngineCall {
    EngineCall::NavigationAnswer {
        view,
        request,
        allow,
    }
}

#[test]
fn ask_holds_the_page_and_the_apps_answer_goes_to_the_engine() {
    // Why: with `Handoff.Ask` the engine waits on the app for every address the policy does not
    // cover; the verdict is made on the engine's thread, the question reaches the app through
    // the queue, and the answer is forwarded.
    let mut fx = Fx::new("ask");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.engine.take();

    let held = fx
        .queue
        .navigate(view, 11, "http://elsewhere.test/a", LINK, true, true);
    assert_eq!(held, Verdict::Ask);
    // Addresses the policy decides alone are not asked about.
    assert_eq!(
        fx.queue
            .navigate(view, 12, "http://10.0.2.2:18081/", LINK, true, true),
        Verdict::Allow
    );
    assert_eq!(
        fx.queue
            .navigate(view, 13, "data:text/html,x", LINK, true, true),
        Verdict::Allow
    );
    assert_eq!(
        fx.queue
            .navigate(view, 14, "file:///etc/passwd", LINK, true, true),
        Verdict::Deny
    );
    // A frame inside the page is never asked about: the page's hidden frames must not be able to
    // make the app answer questions.
    assert_eq!(
        fx.queue
            .navigate(view, 15, "http://elsewhere.test/", LINK, false, false),
        Verdict::Deny
    );
    assert_eq!(
        fx.queue
            .navigate(view, 16, "mailto:a@b", LINK, false, false),
        Verdict::Deny
    );
    fx.pump(5000);
    assert_eq!(
        fx.news(events),
        vec![FEv::Navigation(wire::NavigationRequestedArgs {
            view,
            request: 11,
            url: "http://elsewhere.test/a".into(),
            kind: wire::NAVIGATION_KIND_LINK_CLICKED,
            gesture: true,
        })]
    );
    assert!(
        fx.engine.take().is_empty(),
        "the engine waits for the answer"
    );

    fx.nav_answer(app, view, 11, true).unwrap();
    assert_eq!(fx.engine.take(), vec![answer(view, 11, true)]);
    // The request is spent: a second answer is "no such request", and the engine is not told twice.
    assert_eq!(fx.nav_answer(app, view, 11, false), Err(ESRCH));
    assert_eq!(fx.nav_answer(app, view, 999, true), Err(ESRCH));
    assert!(fx.engine.take().is_empty());
    // Another client's answer is "no such view" and does not consume the request.
    fx.queue
        .navigate(view, 17, "http://elsewhere.test/b", LINK, true, true);
    fx.pump(5001);
    assert_eq!(fx.nav_answer(origin(8), view, 17, true), Err(ENOENT));
    fx.nav_answer(app, view, 17, false).unwrap();
    assert_eq!(fx.engine.take(), vec![answer(view, 17, false)]);
}

#[test]
fn a_yes_loads_only_a_network_address_the_engine_can_load() {
    // Why: `mailto:` is for another program to open; an app that answers yes to a held
    // `mailto:` must not make the engine navigate to it (the engine would fail the load or, worse,
    // pass it on). The app may open it itself.
    let mut fx = Fx::new("ask-yes");
    let app = origin(7);
    let (view, _) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.engine.take();
    for (request, url) in [
        (1, "mailto:a@b"),
        (2, "tel:+3312345"),
        (3, "https://other.test/"),
        (4, "x-app://thing"),
    ] {
        assert_eq!(
            fx.queue.navigate(view, request, url, LINK, true, true),
            Verdict::Ask,
            "{url}"
        );
    }
    fx.pump(0);
    for request in 1..=4 {
        fx.nav_answer(app, view, request, true).unwrap();
    }
    assert_eq!(
        fx.engine.take(),
        vec![
            answer(view, 1, false),
            answer(view, 2, false),
            answer(view, 3, true),
            answer(view, 4, false)
        ]
    );
}

#[test]
fn a_question_nobody_answers_in_ten_seconds_is_a_no() {
    // Why: the engine holds the page's navigation until the service answers; an app that never
    // does (hung, crashed, ignoring the event) must not hold the page forever.
    let mut fx = Fx::new("ask-expiry");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.engine.take();
    fx.queue
        .navigate(view, 1, "http://elsewhere.test/", LINK, true, true);
    fx.pump(5000);
    fx.queue
        .navigate(view, 2, "http://elsewhere.test/2", LINK, true, true);
    fx.pump(9000);
    assert_eq!(fx.news(events).len(), 2);
    fx.nav_answer(app, view, 2, true).unwrap();
    fx.engine.take();

    fx.pump(5000 + HOLD_MS - 1);
    assert!(fx.engine.take().is_empty(), "still within the time");
    fx.pump(5000 + HOLD_MS);
    assert_eq!(fx.engine.take(), vec![answer(view, 1, false)]);
    // Too late: the request is gone, and the engine was told exactly once.
    assert_eq!(fx.nav_answer(app, view, 1, true), Err(ESRCH));
    fx.pump(100_000);
    assert!(
        fx.engine.take().is_empty(),
        "an answered request does not expire again"
    );
    let notes = fx.service.take_notes();
    assert!(notes.contains(&Note::Full {
        view,
        what: "held expired"
    }));
}

#[test]
fn a_view_holds_eight_questions_and_refuses_the_rest_at_once() {
    // Why: each held navigation is an event the app must answer; a page that navigates in a loop
    // must not queue unbounded questions (or the app's answers).
    let mut fx = Fx::new("ask-cap");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Net, asking(), 4, 4);
    let (other, other_events) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.engine.take();
    let total = HELD_PER_VIEW as u64 + 2;
    for request in 1..=total {
        let url = format!("http://elsewhere.test/{request}");
        assert_eq!(
            fx.queue.navigate(view, request, &url, LINK, true, true),
            Verdict::Ask
        );
    }
    fx.queue
        .navigate(other, 100, "http://elsewhere.test/", LINK, true, true);
    fx.pump(0);
    let asked = fx.news(events);
    assert_eq!(asked.len(), HELD_PER_VIEW, "the first eight are asked");
    assert_eq!(
        fx.news(other_events).len(),
        1,
        "another view's count is its own"
    );
    let refused = vec![answer(view, total - 1, false), answer(view, total, false)];
    assert_eq!(fx.engine.take(), refused);
    assert_eq!(
        fx.nav_answer(app, view, total, true),
        Err(ESRCH),
        "refused ones are not held"
    );
    // Answering one makes room for the next.
    fx.nav_answer(app, view, 1, true).unwrap();
    fx.queue
        .navigate(view, 50, "http://elsewhere.test/50", LINK, true, true);
    fx.pump(1);
    assert_eq!(fx.news(events).len(), 1);
    let notes = fx.service.take_notes();
    assert_eq!(
        notes
            .iter()
            .filter(|n| matches!(
                n,
                Note::Full {
                    what: "held refused",
                    ..
                }
            ))
            .count(),
        2
    );
}

#[test]
fn a_question_for_a_view_that_ended_is_refused_not_asked() {
    // Why: the queue is drained after the page asked; the view may be gone by then, and the
    // engine is waiting for an answer either way.
    let mut fx = Fx::new("ask-gone");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.engine.take();
    assert_eq!(
        fx.queue
            .navigate(view, 1, "http://elsewhere.test/", LINK, true, true),
        Verdict::Ask
    );
    fx.destroy_view(app, view).unwrap();
    fx.engine.take();
    fx.pump(0);
    assert_eq!(fx.engine.take(), vec![answer(view, 1, false)]);
    assert!(fx.news(events).is_empty());
    // And a later navigation of an unknown view is refused on the spot.
    assert_eq!(
        fx.queue
            .navigate(view, 2, "http://10.0.2.2:18081/", LINK, true, true),
        Verdict::Deny
    );
}

fn browsing() -> wire::ViewConfig {
    wire::ViewConfig {
        handoff: wire::HANDOFF_BROWSER,
        ..web()
    }
}

#[test]
fn a_hand_off_needs_a_gesture_and_the_users_own_recent_input_through_the_service() {
    // Why: a page's script must never be able to start another program. The only input the
    // service counts is a press, release or key press the client forwarded to this view, within
    // three seconds, on the service's own clock; the page's `gesture` claim is not enough.
    let mut fx = Fx::new("handoff");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, browsing(), 4, 4);
    let (idle, _) = fx.view(app, Via::Browser, browsing(), 4, 4);
    fx.outside.set_open_result(true);
    let hand =
        |fx: &Fx, view, gesture, main| fx.queue.navigate(view, 1, "mailto:a@b", 0, gesture, main);

    fx.set_clock(1000);
    assert_eq!(
        hand(&fx, view, true, true),
        Verdict::Deny,
        "no input at all"
    );
    fx.mouse(app, view, 0).unwrap();
    fx.set_clock(1100);
    assert_eq!(
        hand(&fx, view, true, true),
        Verdict::Deny,
        "a move is not input"
    );
    fx.key(app, view, false).unwrap();
    assert_eq!(
        hand(&fx, view, true, true),
        Verdict::Deny,
        "nor is a key release"
    );
    // Another client cannot stamp the view; the refused call changes nothing.
    assert_eq!(fx.mouse(origin(8), view, 1), Err(ENOENT));
    assert_eq!(hand(&fx, view, true, true), Verdict::Deny);

    fx.set_clock(2000);
    fx.mouse(app, view, 1).unwrap();
    fx.set_clock(2500);
    assert_eq!(
        hand(&fx, view, false, true),
        Verdict::Deny,
        "the page must claim a user gesture"
    );
    assert_eq!(
        hand(&fx, view, true, false),
        Verdict::Deny,
        "a frame in the page never hands off"
    );
    assert_eq!(
        hand(&fx, idle, true, true),
        Verdict::Deny,
        "input is per view"
    );
    assert_eq!(hand(&fx, view, true, true), Verdict::HandOff);
    fx.set_clock(2000 + INPUT_LOCK_MS);
    assert_eq!(
        hand(&fx, view, true, true),
        Verdict::HandOff,
        "at the edge of the window"
    );
    fx.set_clock(2001 + INPUT_LOCK_MS);
    assert_eq!(
        hand(&fx, view, true, true),
        Verdict::Deny,
        "the input is stale"
    );
    // A key press and a mouse release are input too.
    fx.key(app, view, true).unwrap();
    assert_eq!(hand(&fx, view, true, true), Verdict::HandOff);
    fx.set_clock(10_000);
    fx.mouse(app, view, 2).unwrap();
    assert_eq!(hand(&fx, view, true, true), Verdict::HandOff);

    // The queued hand-offs are given to the system, and the client hears of each.
    fx.pump(0);
    assert_eq!(fx.outside.opened(), vec!["mailto:a@b"; 4]);
    let sent = fx.news(events);
    assert_eq!(sent.len(), 4);
    assert!(sent.iter().all(|e| *e
        == FEv::HandedOff(wire::HandedOffArgs {
            view,
            url: "mailto:a@b".into()
        })));
}

#[test]
fn a_hand_off_the_system_would_not_take_is_not_reported_as_made() {
    // Why: `HandedOff` tells the app the address went to another program; saying so when `mimed`
    // refused would make the app show a success that did not happen.
    let mut fx = Fx::new("handoff-refused");
    let app = origin(7);
    let (view, events) = fx.view(app, Via::Browser, browsing(), 4, 4);
    fx.mouse(app, view, 1).unwrap();
    fx.outside.set_open_result(false);
    assert_eq!(
        fx.queue.navigate(view, 1, "tel:123", 0, true, true),
        Verdict::HandOff
    );
    fx.pump(0);
    assert_eq!(fx.outside.opened(), vec!["tel:123"]);
    assert!(fx.news(events).is_empty());
    assert!(fx.service.take_notes().contains(&Note::Full {
        view,
        what: "hand-off refused"
    }));
    fx.outside.set_open_result(true);
    fx.queue.navigate(view, 2, "tel:456", 0, true, true);
    fx.pump(1);
    assert_eq!(fx.news(events).len(), 1);
    // A view that does not hand off asks, or refuses; nothing reaches the system.
    let (asks, _) = fx.view(app, Via::Net, asking(), 4, 4);
    fx.mouse(app, asks, 1).unwrap();
    assert_eq!(
        fx.queue.navigate(asks, 3, "tel:789", 0, true, true),
        Verdict::Ask
    );
    let (denies, _) = fx.view(app, Via::Net, web(), 4, 4);
    fx.mouse(app, denies, 1).unwrap();
    assert_eq!(
        fx.queue.navigate(denies, 4, "tel:789", 0, true, true),
        Verdict::Deny
    );
    fx.pump(2);
    assert_eq!(
        fx.outside.opened().len(),
        2,
        "only the two hand-off views' addresses"
    );
}
