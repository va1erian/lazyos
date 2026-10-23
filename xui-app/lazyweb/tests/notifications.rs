//! Web notifications through the LazyWeb window, driven offscreen against a recording engine
//! and a fake shell: the permission bar, what a page may show and as whom, the tag, the click and
//! the close, and the limits. Snapshots go to `xui-app/target/snapshots/lazyweb-notify-*.png`.

mod support;

use std::fs;
use std::sync::{Mutex, MutexGuard, PoisonError};

use support::*;
use xui_canvas::snapshot::Stage;
use xui_webkit::engine::{Call, NotifyAnswer};
use xui_webkit::events::Event;
use xui_webkit::gesture;
use xui_lazyweb::app::Msg;
use xui_lazyweb::chrome::Command;
use xui_lazyweb::notify::{BarAnswer, Decision, Reason};
use xui_lazyweb::pages::ABOUT;

/// The last real input is one process-wide stamp: the tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> MutexGuard<'static, ()> {
    TURN.lock().unwrap_or_else(PoisonError::into_inner)
}

const SITE: &str = "https://mail.example.com";
const PAGE: &str = "https://mail.example.com/inbox";

fn show(id: u64, tag: &str) -> Event {
    Event::NotifyShow {
        id,
        origin: SITE.into(),
        page_url: PAGE.into(),
        title: "New mail".into(),
        body: "From Ada".into(),
        tag: tag.into(),
    }
}

fn ask(origin: &str) -> Event {
    Event::NotifyPermission {
        origin: origin.into(),
        user_gesture: true,
    }
}

/// The page is on screen and the person clicked it (the input the window forwarded).
fn on_page(stage: &Stage<'_, Msg>, rig: &Rig) {
    rig.state(stage, loaded(PAGE, "Inbox", 1));
    gesture::stamp();
}

/// The person allowed the site.
fn allowed(stage: &Stage<'_, Msg>, rig: &Rig) {
    on_page(stage, rig);
    rig.events(stage, vec![ask(SITE)]);
    stage.emit(Msg::NotifyAnswer(BarAnswer::Allow));
}

fn answers(calls: &[Call]) -> Vec<(String, NotifyAnswer)> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::NotifyPermission(origin, answer) => Some((origin.clone(), *answer)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_request_with_a_gesture_asks_in_a_bar_and_allow_is_kept_and_told_to_the_engine() {
    let _turn = turn();
    let (image, calls) = drive("notify-allow", None, |stage, rig| {
        on_page(stage, rig);
        rig.mailbox.post_frame(880, 400, &test_page(880, 400));
        stage.emit(Msg::Frame);
        rig.events(stage, vec![ask(SITE)]);
        assert!(
            !rig.engine
                .calls()
                .iter()
                .any(|c| matches!(c, Call::NotifyPermission(..))),
            "nobody answered yet"
        );
        assert!(
            !rig.notify_file().exists(),
            "nothing is kept before the answer"
        );
    });
    save(&image, "lazyweb-notify-ask.png");
    assert!(answers(&calls).is_empty());

    let (_, calls) = drive("notify-allow2", None, |stage, rig| {
        allowed(stage, rig);
        assert_eq!(
            fs::read_to_string(rig.notify_file())
                .unwrap()
                .split('\t')
                .collect::<Vec<_>>()[..2],
            ["https://mail.example.com", "allow"]
        );
        assert_eq!(
            rig.permissions.decision("https://mail.example.com"),
            Some(Decision::Allow)
        );
    });
    assert_eq!(
        answers(&calls),
        [("https://mail.example.com".to_owned(), NotifyAnswer::Allow)]
    );
}

#[test]
fn block_is_kept_and_a_blocked_site_shows_nothing() {
    let _turn = turn();
    let (_, calls) = drive("notify-block", None, |stage, rig| {
        on_page(stage, rig);
        rig.events(stage, vec![ask(SITE)]);
        stage.emit(Msg::NotifyAnswer(BarAnswer::Block));
        assert_eq!(
            rig.permissions.decision("https://mail.example.com"),
            Some(Decision::Block)
        );
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        assert!(rig.toasts.posted.borrow().is_empty());
        // Asking again is answered from what was decided, without a bar.
        gesture::stamp();
        rig.events(stage, vec![ask(SITE)]);
    });
    assert_eq!(
        answers(&calls),
        [
            ("https://mail.example.com".to_owned(), NotifyAnswer::Block),
            ("https://mail.example.com".to_owned(), NotifyAnswer::Block),
        ]
    );
}

#[test]
fn not_now_asks_again_later_and_keeps_nothing() {
    let _turn = turn();
    let (_, calls) = drive("notify-later", None, |stage, rig| {
        on_page(stage, rig);
        rig.events(stage, vec![ask(SITE)]);
        stage.emit(Msg::NotifyAnswer(BarAnswer::Later));
        assert!(!rig.notify_file().exists());
        assert_eq!(rig.permissions.decision("https://mail.example.com"), None);
    });
    assert_eq!(
        answers(&calls),
        [("https://mail.example.com".to_owned(), NotifyAnswer::Dismiss)]
    );
}

#[test]
fn a_request_without_a_gesture_or_from_a_page_with_no_site_is_refused_without_a_bar() {
    let _turn = turn();
    let (_, calls) = drive("notify-refuse", None, |stage, rig| {
        rig.state(stage, loaded(PAGE, "Inbox", 1));
        // No input was forwarded to the page: the engine's flag alone is not trusted.
        std::thread::sleep(std::time::Duration::from_millis(1));
        let _ = gesture::take_recent(std::time::Duration::from_secs(60));
        rig.events(stage, vec![ask(SITE)]);
        rig.events(
            stage,
            vec![Event::NotifyPermission {
                origin: SITE.into(),
                user_gesture: false,
            }],
        );
        gesture::stamp();
        rig.events(stage, vec![ask("data:text/html,x"), ask("about:blank")]);
        rig.events(stage, vec![ask("file:///tmp/x.html")]);
        assert!(rig.permissions.sites().is_empty());
        assert!(!rig.notify_file().exists());
    });
    let got = answers(&calls);
    assert_eq!(got.len(), 5, "{got:?}");
    assert!(got
        .iter()
        .all(|(_, answer)| *answer == NotifyAnswer::Dismiss));
}

#[test]
fn a_site_that_was_not_allowed_cannot_post() {
    let _turn = turn();
    let (_, calls) = drive("notify-unallowed", None, |stage, rig| {
        on_page(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        assert!(rig.toasts.posted.borrow().is_empty());
    });
    assert!(!calls.iter().any(|c| matches!(c, Call::NotifyShown(_))));
}

#[test]
fn an_allowed_site_shows_a_toast_titled_with_its_host_and_never_its_own_words() {
    let _turn = turn();
    let (_, calls) = drive("notify-show", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(
            stage,
            vec![Event::NotifyShow {
                id: 5,
                origin: SITE.into(),
                page_url: PAGE.into(),
                title: "Windows Security\u{202E}\nAlert".into(),
                body: "Your PC\u{7}\nis infected".into(),
                tag: String::new(),
            }],
        );
        rig.shell(stage);
        let posted = rig.toasts.posted.borrow();
        assert_eq!(posted.len(), 1);
        let (key, toast) = &posted[0];
        assert_eq!(*key, 5);
        assert_eq!(toast.title, "mail.example.com");
        assert_eq!(toast.body, "Windows Security Alert\nYour PC is infected");
        assert_eq!(toast.level, xui_lazyweb::transfers::ToastLevel::Info);
        assert!(!toast.sticky && toast.replaces.is_none());
    });
    // Once the shell had it up, the page's `show` event fired.
    assert!(calls.contains(&Call::NotifyShown(5)));
}

#[test]
fn a_tag_updates_the_toast_in_place_and_closes_the_pages_old_object() {
    let _turn = turn();
    let (_, calls) = drive("notify-tag", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "chat")]);
        rig.shell(stage);
        let first = rig.toasts.posted.borrow()[0].1.replaces;
        assert_eq!(first, None);
        // Later, the same tag from the same site.
        rig.events(stage, vec![show(2, "chat")]);
        rig.shell(stage);
        let posted = rig.toasts.posted.borrow();
        assert_eq!(posted.len(), 2);
        assert_eq!(posted[1].1.replaces, Some(101), "the same toast, updated");
    });
    assert!(calls.contains(&Call::NotifyClosed(1)), "{calls:?}");
    assert!(calls.contains(&Call::NotifyShown(2)));
}

#[test]
fn a_click_runs_the_pages_handler_when_the_page_is_still_there() {
    let _turn = turn();
    let (_, calls) = drive("notify-click", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        rig.toasts.click(101);
        rig.shell(stage);
    });
    assert!(calls.contains(&Call::NotifyClicked(1)), "{calls:?}");
    // The page is the one on show: nothing is loaded for the click.
    assert!(
        !calls
            .iter()
            .any(|c| matches!(c, Call::Load(u) if u == PAGE)),
        "{calls:?}"
    );
}

#[test]
fn a_click_on_a_toast_of_a_page_that_has_gone_brings_the_page_back() {
    let _turn = turn();
    let (_, calls) = drive("notify-gone", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        // The person went elsewhere: the page's object is gone, the toast stays.
        rig.state(stage, loading("https://other.example.org/"));
        rig.state(stage, loaded("https://other.example.org/", "Other", 2));
        rig.events(stage, vec![Event::NotifyGone { id: 1 }]);
        rig.toasts.click(101);
        rig.shell(stage);
    });
    assert!(calls.contains(&Call::Load(PAGE.into())), "{calls:?}");
    assert!(
        !calls.contains(&Call::NotifyClicked(1)),
        "no script is left to run: {calls:?}"
    );
}

#[test]
fn a_notification_whose_site_is_not_the_page_on_screen_is_refused() {
    let _turn = turn();
    // The page's process says which origin a notification is for; the UI process says which
    // page that process is showing. A mismatch is a lie, never a toast titled with a site.
    let (_, calls) = drive("notify-away", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(
            stage,
            vec![Event::NotifyShow {
                id: 1,
                origin: SITE.into(),
                // Not a page of the site that notified.
                page_url: "https://evil.example.org/phish".into(),
                title: "t".into(),
                body: "b".into(),
                tag: String::new(),
            }],
        );
        rig.shell(stage);
        assert!(rig.toasts.posted.borrow().is_empty());
    });
    assert!(!calls.iter().any(|c| matches!(c, Call::NotifyShown(_))));
}

#[test]
fn a_bar_never_names_a_site_other_than_the_page_on_screen() {
    let _turn = turn();
    let (_, calls) = drive("notify-lie", None, |stage, rig| {
        // The person is on evil.example.org; its process claims to be asking for the bank.
        rig.state(stage, loaded("https://evil.example.org/", "Evil", 1));
        gesture::stamp();
        rig.events(stage, vec![ask("https://bank.example")]);
        assert!(rig.permissions.sites().is_empty());
        assert!(!rig.notify_file().exists());
    });
    assert_eq!(
        answers(&calls),
        [("https://bank.example".to_owned(), NotifyAnswer::Dismiss)]
    );
}

#[test]
fn a_request_the_engine_gave_up_on_is_not_asked_any_more() {
    let _turn = turn();
    let (_, calls) = drive("notify-gone-ask", None, |stage, rig| {
        on_page(stage, rig);
        rig.events(stage, vec![ask(SITE)]);
        rig.events(
            stage,
            vec![Event::NotifyPermissionGone {
                origin: SITE.into(),
            }],
        );
        // Nothing is asked: Allow now finds no question.
        stage.emit(Msg::NotifyAnswer(BarAnswer::Allow));
        assert!(rig.permissions.sites().is_empty());
    });
    assert!(answers(&calls).is_empty(), "{calls:?}");
}

#[test]
fn a_toast_the_person_dismisses_closes_the_pages_notification() {
    let _turn = turn();
    let (_, calls) = drive("notify-close", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, ""), show(2, "")]);
        rig.shell(stage);
        rig.toasts.end(101, Reason::User);
        rig.toasts.end(102, Reason::Timeout);
        rig.shell(stage);
    });
    assert!(calls.contains(&Call::NotifyClosed(1)));
    assert!(calls.contains(&Call::NotifyClosed(2)));
}

#[test]
fn a_page_process_that_crashed_leaves_toasts_that_reopen_the_page() {
    let _turn = turn();
    let (_, calls) = drive("notify-crash", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        let mut crashed = loaded(PAGE, "Inbox", 1);
        crashed.crashed = true;
        rig.state(stage, crashed);
        rig.toasts.click(101);
        rig.shell(stage);
    });
    assert!(!calls.contains(&Call::NotifyClicked(1)), "{calls:?}");
    assert!(calls.contains(&Call::Load(PAGE.into())), "{calls:?}");
}

#[test]
fn the_page_taking_its_notification_down_takes_the_toast_down() {
    let _turn = turn();
    let (_, calls) = drive("notify-cancel", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        rig.events(stage, vec![Event::NotifyCancel { id: 1 }]);
        assert_eq!(*rig.toasts.closed.borrow(), [101]);
        // The page was told by its own `close()`; the engine is not told again.
    });
    assert!(!calls.contains(&Call::NotifyClosed(1)));
}

#[test]
fn a_site_that_posts_in_a_burst_gets_a_few_toasts_and_the_rest_are_dropped() {
    let _turn = turn();
    let (_, calls) = drive("notify-burst", None, |stage, rig| {
        allowed(stage, rig);
        let burst = (1..=6).map(|id| show(id, &format!("t{id}"))).collect();
        rig.events(stage, burst);
        rig.shell(stage);
        assert_eq!(rig.toasts.posted.borrow().len(), 3);
    });
    // The three that were dropped are told closed: their page objects are not left "showing".
    let closed: Vec<_> = calls
        .iter()
        .filter(|c| matches!(c, Call::NotifyClosed(_)))
        .collect();
    assert_eq!(closed.len(), 3, "{calls:?}");
}

#[test]
fn a_shell_that_refuses_shows_nothing_and_the_page_hears_nothing() {
    let _turn = turn();
    let (_, calls) = drive("notify-refused", None, |stage, rig| {
        allowed(stage, rig);
        rig.toasts.refuse.set(true);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
    });
    assert!(!calls.iter().any(|c| matches!(c, Call::NotifyShown(_))));
}

#[test]
fn the_request_of_a_page_that_is_gone_is_dismissed() {
    let _turn = turn();
    let (_, calls) = drive("notify-moved", None, |stage, rig| {
        on_page(stage, rig);
        rig.events(stage, vec![ask(SITE)]);
        rig.state(stage, loaded("https://other.example.org/", "Other", 2));
    });
    assert_eq!(
        answers(&calls),
        [("https://mail.example.com".to_owned(), NotifyAnswer::Dismiss)]
    );
}

#[test]
fn the_about_page_lists_the_decisions_and_forgetting_one_tells_the_engine() {
    let _turn = turn();
    let (image, calls) = drive("notify-about", None, |stage, rig| {
        allowed(stage, rig);
        rig.events(stage, vec![show(1, "")]);
        rig.shell(stage);
        stage.emit(Msg::Menu(Command::About));
        let url = rig.internal.url_of(ABOUT);
        assert!(url.contains("forget-notify"), "listed on the page");
        rig.state(stage, loaded(&url, "About LazyWeb", 2));
        rig.events(
            stage,
            vec![Event::Command {
                url: "x-lazyweb:forget-notify/https%3A%2F%2Fmail.example.com".into(),
            }],
        );
        assert_eq!(rig.permissions.decision("https://mail.example.com"), None);
        assert_eq!(
            *rig.toasts.closed.borrow(),
            [101],
            "its live toast went with it"
        );
    });
    save(&image, "lazyweb-notify-about.png");
    assert!(calls.contains(&Call::NotifyForget("https://mail.example.com".into())));
}
