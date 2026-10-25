//! What a page and the engine may start, driven through the LazyWeb window offscreen against a
//! recording engine: another app, a command of the browser's, a download, a JavaScript dialog, a
//! new window. Snapshots go to `xui-app/target/snapshots/lazyweb-*.png`.

mod support;

use std::fs;

use support::*;
use xui_canvas::snapshot::Stage;
use xui_core::backend::Event as UiEvent;
use xui_core::Key;
use xui_lazyweb::app::Msg;
use xui_lazyweb::chrome::Command;
use xui_lazyweb::pages::HISTORY;
use xui_webkit::engine::{Call, DialogKind};
use xui_webkit::events::Event;

#[test]
fn a_link_for_another_app_goes_to_it_only_when_a_person_started_it() {
    let (_, calls) = drive("launch", None, |stage, rig| {
        rig.events(
            stage,
            vec![
                Event::Launch {
                    url: "mailto:a@b.test".into(),
                    by_user: true,
                },
                Event::Launch {
                    url: "tel:5551234".into(),
                    by_user: false,
                },
            ],
        );
        assert_eq!(*rig.opened.urls.borrow(), ["mailto:a@b.test"]);
    });
    // Nothing was loaded because of either.
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.contains("mailto") || u.contains("tel:"))));
}

#[test]
fn a_typed_scheme_for_another_app_is_the_users_own_act() {
    let (image, calls, opened) = drive_full("typed", None, |stage, _| {
        // Ctrl+L clears the field; a click in it (below the menu bar, 28, and the tab strip, 30, in
        // the toolbar, 38) gives it the keyboard the headless stage has not given it yet.
        stage.emit(Msg::FocusAddress);
        stage.click(300, 77);
        for c in "mailto:me@example.com".chars() {
            stage.inject(UiEvent::Char(c));
        }
        stage.emit(Msg::Go);
    });
    save(&image, "lazyweb-typed.png");
    assert_eq!(*opened.urls.borrow(), ["mailto:me@example.com"]);
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.starts_with("mailto"))));
}

#[test]
fn a_command_runs_from_the_history_page_but_not_from_a_web_page() {
    let (_, _) = drive("command", None, |stage, rig| {
        // A visit to forget.
        rig.state(stage, loading("http://example.com/"));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        assert!(rig.history_file().exists());

        // On a web page the command is refused, whoever let it through.
        rig.events(
            stage,
            vec![Event::Command {
                url: "x-lazyweb:clear-history".into(),
            }],
        );
        assert!(fs::read_to_string(rig.history_file())
            .unwrap()
            .contains("example.com"));

        // On the history page it runs.
        stage.emit(Msg::Menu(Command::ShowHistory));
        let url = rig.internal.url_of(HISTORY);
        rig.state(stage, loaded(&url, "History", 2));
        rig.events(
            stage,
            vec![Event::Command {
                url: "x-lazyweb:clear-history".into(),
            }],
        );
        assert!(!fs::read_to_string(rig.history_file())
            .unwrap()
            .contains("example.com"));
    });
}

#[test]
fn a_download_runs_shows_in_the_status_bar_completes_and_is_listed() {
    let (running, _) = drive("download", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(
            stage,
            vec![
                Event::DownloadStarted {
                    id: 7,
                    url: "https://theoldnet.com/files/oldnet-kit.zip".into(),
                    name: "oldnet-kit.zip".into(),
                    total: Some(307_200),
                },
                Event::DownloadProgress {
                    id: 7,
                    received: 153_600,
                    total: Some(307_200),
                },
            ],
        );
    });
    save(&running, "lazyweb-download-running.png");
}
#[test]
fn a_dialog_is_drawn_over_the_page_and_waits_for_the_person() {
    let (image, calls) = drive(
        "dialog-open",
        Some("https://theoldnet.com/"),
        |stage, rig| {
            rig.mailbox.post_frame(880, 400, &test_page(880, 400));
            stage.emit(Msg::Frame);
            ask(stage, rig, 41, DialogKind::Confirm);
        },
    );
    save(&image, "lazyweb-dialog.png");
    assert!(
        !calls.iter().any(|c| matches!(c, Call::DialogReply(..))),
        "nobody answered yet: {calls:?}"
    );
}

#[test]
fn enter_accepts_a_dialog_and_the_answer_goes_to_the_engine() {
    let (_, calls) = drive("dialog", Some("https://theoldnet.com/"), |stage, rig| {
        ask(stage, rig, 41, DialogKind::Confirm);
        // The dialog's OK has the focus.
        stage.inject(key_down(Key::RETURN));
    });
    assert!(
        calls.contains(&Call::DialogReply(41, true, String::new())),
        "{calls:?}"
    );
}

#[test]
fn escape_cancels_a_dialog() {
    let (_, calls) = drive("dialog-esc", None, |stage, rig| {
        rig.events(
            stage,
            vec![Event::Dialog {
                id: 5,
                kind: DialogKind::Confirm,
                text: "Sure?".into(),
                default: String::new(),
            }],
        );
        stage.inject(key_down(Key::ESCAPE));
    });
    assert!(
        calls.contains(&Call::DialogReply(5, false, String::new())),
        "{calls:?}"
    );
}

#[test]
fn a_new_window_request_opens_a_tab_but_a_scheme_for_another_app_does_not() {
    let (_, calls) = drive("popup", None, |stage, rig| {
        rig.events(
            stage,
            vec![
                Event::Popup {
                    url: "https://theoldnet.com/other.html".into(),
                },
                Event::Popup {
                    url: "mailto:x@y.test".into(),
                },
            ],
        );
        assert!(rig.opened.urls.borrow().is_empty());
        // A second tab, in front, with its own engine; the first tab's page was left alone.
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), 2, "only the https window request made a tab");
        assert!(made[1]
            .engine
            .calls()
            .contains(&Call::Load("https://theoldnet.com/other.html".into())));
        assert_eq!(*rig.engines.fronts.borrow(), [0, 1]);
    });
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.contains("theoldnet.com/other") || u.starts_with("mailto"))));
}

fn ask(stage: &Stage<'_, Msg>, rig: &Rig, id: u64, kind: DialogKind) {
    rig.state(stage, loaded("https://theoldnet.com/x", "X", 1));
    rig.events(
        stage,
        vec![Event::Dialog {
            id,
            kind,
            text: "Delete everything?".into(),
            default: String::new(),
        }],
    );
}
