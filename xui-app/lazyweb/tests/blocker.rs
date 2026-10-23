//! The ad blocker in the window, driven offscreen against a recording engine: `about:blocker`, the
//! commands on it, "Reload without blocking", and the engine's news about blocked loads. The
//! engine's half (the rule lists) is checked against the real WebKit by
//! `python tools/webkit/blockcheck.py`.

mod support;

use support::*;
use xui_webkit::abi::ListStatus;
use xui_webkit::engine::Call;
use xui_webkit::events::Event;
use xui_lazyweb::app::Msg;
use xui_lazyweb::blocker::ListState;
use xui_lazyweb::chrome::Command;

/// The addresses the window asked the engine to load, in order.
fn loads(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn the_blocker_page_opens_from_the_menu_and_names_the_site_browsed_last() {
    let (image, calls) = drive(
        "blocker-page",
        Some("https://news.example/"),
        |stage, rig| {
            rig.state(stage, loaded("https://news.example/", "News", 1));
            stage.emit(Msg::Menu(Command::Blocker));
            let page = rig.engine.calls();
            let url = loads(&page).pop().expect("the page was loaded");
            assert_eq!(rig.internal.name_for(&url), Some("about:blocker"), "{url}");
            // The page is a data: URL: what it offers is in the HTML.
            let html = String::from_utf8_lossy(&support_decode(&url)).into_owned();
            assert!(html.contains("x-lazyweb:site-off/news.example"), "{html}");
            assert!(html.contains("x-lazyweb:blocker-off"));
            // The page itself, for a person to open in a browser and look at.
            let dir =
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/snapshots");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("lazyweb-blocker-page.html"), html).unwrap();
        },
    );
    save(&image, "lazyweb-blocker-page.png");
    assert!(!loads(&calls).is_empty());
}

#[test]
fn a_command_on_the_page_changes_the_settings_and_builds_the_page_again() {
    drive(
        "blocker-site",
        Some("https://news.example/"),
        |stage, rig| {
            rig.state(stage, loaded("https://news.example/", "News", 1));
            stage.emit(Msg::Menu(Command::Blocker));
            let first = loads(&rig.engine.calls()).pop().unwrap();
            // The engine reports our page as the one on show; then the user clicks a button on it.
            rig.state(stage, loaded(&first, "Ad blocker", 2));
            rig.events(
                stage,
                vec![Event::Command {
                    url: "x-lazyweb:site-off/news.example".into(),
                }],
            );
            assert!(rig.blocker.site_excepted("news.example"));
            assert!(!rig.blocker.decide("https://news.example/story"));
            let again = loads(&rig.engine.calls()).pop().unwrap();
            let html = String::from_utf8_lossy(&support_decode(&again)).into_owned();
            assert!(
                html.contains("x-lazyweb:site-on/news.example"),
                "the page shows the change"
            );

            rig.state(stage, loaded(&again, "Ad blocker", 3));
            rig.events(
                stage,
                vec![Event::Command {
                    url: "x-lazyweb:blocker-off".into(),
                }],
            );
            assert!(!rig.blocker.settings().enabled);
            assert!(!rig.blocker.decide("https://other.example/"));
        },
    );
}

#[test]
fn a_web_page_cannot_run_the_blockers_commands() {
    drive(
        "blocker-forged",
        Some("https://evil.example/"),
        |stage, rig| {
            rig.state(stage, loaded("https://evil.example/", "Evil", 1));
            rig.events(
                stage,
                vec![
                    Event::Command {
                        url: "x-lazyweb:blocker-off".into(),
                    },
                    Event::Command {
                        url: "x-lazyweb:site-off/evil.example".into(),
                    },
                ],
            );
            assert!(rig.blocker.settings().enabled);
            assert!(rig.blocker.settings().exceptions.is_empty());
            assert!(rig.blocker.decide("https://evil.example/"));
        },
    );
}

#[test]
fn reloading_without_blocking_lasts_until_the_site_is_left() {
    let (_, calls) = drive(
        "blocker-once",
        Some("https://news.example/"),
        |stage, rig| {
            rig.state(stage, loaded("https://news.example/story", "Story", 1));
            assert!(rig.blocker.decide("https://news.example/story"));
            stage.emit(Msg::Menu(Command::ReloadUnblocked));
            assert_eq!(rig.blocker.once().as_deref(), Some("news.example"));
            assert!(!rig.blocker.decide("https://news.example/story"));
            assert!(
                rig.blocker.decide("https://other.example/"),
                "only that site"
            );

            // The engine commits the page again: still on the site, still unblocked.
            rig.state(stage, loaded("https://news.example/story", "Story", 2));
            assert_eq!(rig.blocker.once().as_deref(), Some("news.example"));
            // Leaving it ends the exception.
            rig.state(stage, loaded("https://other.example/", "Other", 3));
            assert_eq!(rig.blocker.once(), None);
            assert!(rig.blocker.decide("https://news.example/story"));
        },
    );
    assert!(loads(&calls)
        .iter()
        .any(|u| u == "https://news.example/story"));
}

#[test]
fn the_engines_news_reaches_the_blocker() {
    drive(
        "blocker-news",
        Some("https://news.example/"),
        |stage, rig| {
            rig.state(stage, loaded("https://news.example/", "News", 1));
            rig.events(stage, vec![Event::Blocked(7)]);
            assert_eq!(rig.blocker.blocked(), 7);
            // Opening our own page starts the engine's count again; the page browsed last keeps
            // its number.
            rig.state(stage, loaded("about:blocker", "Ad blocker", 2));
            rig.events(stage, vec![Event::Blocked(0)]);
            assert_eq!(rig.blocker.blocked(), 7);
            // A web page that commits does start again.
            rig.state(stage, loaded("https://news.example/", "News", 3));
            rig.events(stage, vec![Event::Blocked(0)]);
            assert_eq!(rig.blocker.blocked(), 0);
            // A list's answer is the worker's business; with none connected it changes nothing here.
            rig.events(
                stage,
                vec![Event::BlockingList {
                    id: "easylist-00-0".into(),
                    status: ListStatus::Ready,
                }],
            );
            assert_eq!(rig.blocker.lists()[0].state, ListState::Waiting);
        },
    );
}

#[test]
fn our_own_pages_are_never_blocked() {
    drive("blocker-own", None, |_, rig| {
        for url in [
            "about:blocker",
            "about:history",
            "about:blank",
            "data:text/html,x",
        ] {
            assert!(!rig.blocker.decide(url), "{url}");
        }
    });
}
