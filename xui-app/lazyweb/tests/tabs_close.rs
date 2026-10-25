//! What closing a tab ends, and what a tab that lost its view shows (docs/lazyweb-plan.md 2.2):
//! the downloads and notifications a closed tab numbered, and the crashed flag that follows the
//! view and nothing else.

mod support;

use support::*;
use xui_core::Key;
use xui_lazyweb::app::Msg;
use xui_lazyweb::notify::BarAnswer;
use xui_webkit::engine::Call;
use xui_webkit::events::Event;
use xui_webkit::gesture;
use xui_webkit::mailbox::PageState;

const SITE: &str = "https://mail.example.com";
const PAGE: &str = "https://mail.example.com/inbox";

/// The text of the last page the window built for tab `tab` (a `data:` address it loaded).
fn built_page(rig: &Rig, tab: u64) -> String {
    let calls = rig.engines.calls(tab);
    let url = loads(&calls)
        .into_iter()
        .rfind(|url| url.starts_with("data:"))
        .expect("a page of the browser's own");
    String::from_utf8_lossy(&support_decode(&url)).into_owned()
}

#[test]
fn closing_a_tab_ends_its_downloads_and_leaves_its_toasts_to_open_the_site() {
    drive("tab-close-transfers", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        stage.emit(Msg::RestoreAddress);
        rig.engines.state(stage, 1, loaded(PAGE, "Inbox", 1), true);
        gesture::stamp();
        rig.engines.events(
            stage,
            1,
            vec![
                Event::NotifyPermission {
                    origin: SITE.into(),
                    user_gesture: true,
                },
                Event::DownloadStarted {
                    id: 7,
                    url: "https://mail.example.com/big.zip".into(),
                    name: "big.zip".into(),
                    total: Some(1000),
                },
            ],
            true,
        );
        stage.emit(Msg::NotifyAnswer(BarAnswer::Allow));
        rig.engines.events(
            stage,
            1,
            vec![Event::NotifyShow {
                id: 5,
                origin: SITE.into(),
                page_url: PAGE.into(),
                title: "New mail".into(),
                body: "From Ada".into(),
                tag: String::new(),
            }],
            true,
        );
        rig.shell(stage);
        let toast = rig.toasts.posted.borrow().len();
        assert_eq!(toast, 1, "the toast is up");

        // The tab closes with the download unfinished: no event will end it, so the window does.
        stage.inject(chord(Key::W, false));
        assert_eq!(*rig.engines.gone.borrow(), [1]);
        stage.inject(chord(Key::J, false));
        let page = built_page(rig, 0);
        assert!(page.contains("big.zip"), "{page}");
        assert!(page.contains("Failed: its tab was closed"), "{page}");

        // The notification's page is gone with the tab: a click on its toast opens the site
        // (it used to be sent to an engine that no longer existed, and lost).
        rig.toasts.click(101);
        rig.shell(stage);
        assert_eq!(
            loads(&rig.engines.calls(0)).last().map(String::as_str),
            Some(PAGE)
        );
    });
}

fn crashed_state(url: &str) -> PageState {
    PageState {
        url: url.into(),
        committed_url: url.into(),
        crashed: true,
        error: "The page's process crashed".into(),
        ..PageState::default()
    }
}

#[test]
fn a_lost_view_is_shown_until_a_new_one_and_a_switch_does_not_bring_it_back() {
    drive("tab-crash-flag", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines
            .state(stage, 1, loaded("https://two.example/", "Two", 1), true);
        rig.engines
            .state(stage, 0, crashed_state("https://one.example/"), false);
        stage.inject(chord(Key::DIGIT1, false));
        assert!(rig.rects.borrow().contains_key("hang_bar"));
        // Events and states with nothing new in them leave the crashed page as it is.
        stage.emit(Msg::State);
        stage.emit(Msg::Events);
        rig.rects.borrow_mut().clear();
        stage.inject(chord(Key::DIGIT2, false));
        stage.inject(chord(Key::DIGIT1, false));
        assert!(
            rig.rects.borrow().contains_key("hang_bar"),
            "still lost after a round trip"
        );
        // Reload makes a new view; its first state ends the loss for good.
        let (x, y) = rig.centre("hang_reload");
        stage.click(x, y);
        assert!(rig.engines.calls(0).contains(&Call::Reload));
        rig.engines
            .state(stage, 0, loaded("https://one.example/", "One", 2), true);
        rig.rects.borrow_mut().clear();
        stage.inject(chord(Key::DIGIT2, false));
        stage.inject(chord(Key::DIGIT1, false));
        assert!(
            !rig.rects.borrow().contains_key("hang_bar"),
            "the new view's tab is not shown as lost again"
        );
    });
}
