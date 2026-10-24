//! The window's tabs (docs/lazyweb-plan.md 2.6) driven offscreen against a recording engine per tab:
//! Ctrl+T/W/Shift+T/Tab, the strip's clicks, a link opened in a background tab (the keys and the
//! pointer), a page's new window, a tab behind the front one that stops, dialogs from tabs behind,
//! discarding past the limit, and the session of several tabs. Snapshots go to
//! `xui-app/target/snapshots/lazyweb-tabs*.png`.

mod support;

use std::fs;
use std::path::Path;

use support::*;
use xui_core::backend::Event as UiEvent;
use xui_core::{Key, Modifiers, MouseButton};
use xui_lazyweb::app::{Host, Msg};
use xui_lazyweb::prefs::{Prefs, Restore};
use xui_lazyweb::session::{self, Session, Tab, Window};
use xui_lazyweb::strip::TabMsg;
use xui_webkit::engine::{Call, DialogKind};
use xui_webkit::events::Event;
use xui_webkit::mailbox::PageState;

fn chord(key: Key, shift: bool) -> UiEvent {
    UiEvent::KeyDown {
        key,
        modifiers: Modifiers {
            ctrl: true,
            shift,
            ..Modifiers::NONE
        },
        repeat: 1,
        system: false,
    }
}

fn mouse(down: bool, button: MouseButton, x: i32, y: i32, ctrl: bool) -> UiEvent {
    let modifiers = Modifiers {
        ctrl,
        ..Modifiers::NONE
    };
    if down {
        UiEvent::MouseDown {
            x,
            y,
            button,
            modifiers,
        }
    } else {
        UiEvent::MouseUp {
            x,
            y,
            button,
            modifiers,
        }
    }
}

fn loads(calls: &[Call]) -> Vec<String> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect()
}

fn visibility(calls: &[Call]) -> Vec<bool> {
    calls
        .iter()
        .filter_map(|c| match c {
            Call::SetVisible(on) => Some(*on),
            _ => None,
        })
        .collect()
}

/// A page loaded in the first tab, so the window has an address to show and keep.
fn first_page(stage: &xui_canvas::snapshot::Stage<'_, Msg>, rig: &Rig) {
    rig.state(stage, loaded("https://one.example/", "One", 1));
}

#[test]
fn ctrl_t_opens_a_tab_in_front_and_the_first_one_goes_behind() {
    let (image, _) = drive("tab-new", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), 2, "one view of webd per tab");
        assert_eq!(
            made[1].key, "",
            "the start page has no site to put in a process"
        );
        assert!(loads(&made[1].engine.calls())[0].starts_with("data:"));
        assert_eq!(*rig.engines.fronts.borrow(), [0, 1]);
        // The first tab's page is hidden (its slots go back to webd), the new one's shown.
        assert_eq!(visibility(&made[0].engine.calls()), [true, false]);
        assert_eq!(visibility(&made[1].engine.calls()), [true]);
        assert!(
            rig.rects.borrow().contains_key("tab_2"),
            "the strip has two tabs"
        );
        assert!(!rig.rects.borrow().contains_key("tab_3"));
    });
    save(&image, "lazyweb-tabs-new.png");
}

#[test]
fn the_strips_new_tab_button_and_tabs_work_with_the_pointer() {
    drive("tab-strip", None, |stage, rig| {
        first_page(stage, rig);
        let (x, y) = rig.centre("tab_new");
        stage.click(x, y);
        assert_eq!(rig.engines.made.borrow().len(), 2);
        // Back to the first tab by clicking it, away from its close button.
        let tab = rig.rects.borrow()["tab_1"];
        stage.click(tab.left + 30, (tab.top + tab.bottom) / 2);
        assert_eq!(*rig.engines.fronts.borrow(), [0, 1, 0]);
        // The close button of the second tab closes it; its engine is let go.
        let (x, y) = rig.centre("tab_2_close");
        stage.click(x, y);
        assert_eq!(*rig.engines.gone.borrow(), [1]);
        assert!(!rig.rects.borrow()["tab_1"].is_empty());
    });
}

#[test]
fn ctrl_tab_cycles_and_ctrl_digits_jump_and_hide_the_tab_left() {
    drive("tab-cycle", None, |stage, rig| {
        first_page(stage, rig);
        for _ in 0..2 {
            stage.inject(chord(Key::T, false));
        }
        assert_eq!(*rig.engines.fronts.borrow(), [0, 1, 2]);
        stage.inject(chord(Key::TAB, false));
        assert_eq!(
            rig.engines.fronts.borrow().last(),
            Some(&0),
            "past the last: the first"
        );
        stage.inject(chord(Key::TAB, true));
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&2));
        stage.inject(chord(Key::DIGIT2, false));
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&1));
        stage.inject(chord(Key::DIGIT9, false));
        assert_eq!(
            rig.engines.fronts.borrow().last(),
            Some(&2),
            "Ctrl+9 is the last tab"
        );
        // Each switch hid one tab and showed another: no tab was shown twice in a row.
        let made = rig.engines.made.borrow();
        for tab in made.iter() {
            let seen = visibility(&tab.engine.calls());
            assert!(seen.windows(2).all(|pair| pair[0] != pair[1]), "{seen:?}");
        }
    });
}

#[test]
fn ctrl_w_closes_the_tab_and_the_neighbour_comes_to_the_front_and_ctrl_shift_t_brings_it_back() {
    drive("tab-close", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines
            .state(stage, 1, loaded("https://two.example/page", "Two", 1), true);
        stage.inject(chord(Key::W, false));
        assert_eq!(*rig.engines.gone.borrow(), [1]);
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&0));
        // The tab comes back with its page, as a new view that loads the address it was on.
        stage.inject(chord(Key::T, true));
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), 3);
        assert_eq!(loads(&made[2].engine.calls()), ["https://two.example/page"]);
        assert_eq!(made[2].key, "two.example");
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&2));
    });
}

#[test]
fn a_link_opens_in_a_background_tab_in_the_hosts_process_and_the_front_page_stays() {
    drive("tab-link", None, |stage, rig| {
        first_page(stage, rig);
        rig.events(
            stage,
            vec![Event::Hover("https://News.Example:8443/story?id=1".into())],
        );
        stage.emit(Msg::Tab(TabMsg::OpenHovered));
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), 2);
        assert_eq!(made[1].key, "news.example");
        let calls = made[1].engine.calls();
        assert_eq!(loads(&calls), ["https://News.Example:8443/story?id=1"]);
        assert_eq!(
            visibility(&calls),
            [false],
            "it is made hidden: no slots, no paints"
        );
        assert_eq!(
            *rig.engines.fronts.borrow(),
            [0],
            "the front tab is the same"
        );
        assert!(rig.rects.borrow().contains_key("tab_2"));
    });
}

#[test]
fn a_middle_click_or_ctrl_click_on_a_link_opens_a_tab_and_the_page_never_sees_it() {
    let (_, calls) = drive("tab-click", None, |stage, rig| {
        first_page(stage, rig);
        let (ox, oy) = page_origin(stage);
        rig.events(stage, vec![Event::Hover("https://other.example/".into())]);
        rig.engine.take();
        for (button, ctrl) in [(MouseButton::Middle, false), (MouseButton::Left, true)] {
            stage.inject(mouse(true, button, ox + 30, oy + 30, ctrl));
            stage.inject(mouse(false, button, ox + 30, oy + 30, ctrl));
        }
        assert_eq!(rig.engines.made.borrow().len(), 3, "two clicks, two tabs");
        // A plain click is the page's.
        stage.inject(mouse(true, MouseButton::Left, ox + 30, oy + 30, false));
        stage.inject(mouse(false, MouseButton::Left, ox + 30, oy + 30, false));
        assert_eq!(rig.engines.made.borrow().len(), 3);
        // No link under the pointer, no tab.
        rig.events(stage, vec![Event::Hover(String::new())]);
        stage.inject(mouse(true, MouseButton::Middle, ox + 30, oy + 30, false));
        assert_eq!(rig.engines.made.borrow().len(), 3);
        // A script's address is a plain click.
        rig.events(stage, vec![Event::Hover("javascript:void(0)".into())]);
        stage.inject(mouse(true, MouseButton::Left, ox + 30, oy + 30, true));
        assert_eq!(rig.engines.made.borrow().len(), 3);
    });
    let presses = calls
        .iter()
        .filter(
            |c| matches!(c, Call::Mouse(kind, ..) if *kind != xui_webkit::engine::MouseKind::Move),
        )
        .count();
    // The plain click's two events, the middle press with no link (autoscroll takes it: none) and
    // the script link's press: the page saw no press or release of the tab-opening clicks.
    assert!(presses <= 4, "{calls:?}");
}

#[test]
fn a_new_window_request_opens_a_tab_beside_the_opener_behind_when_the_opener_is() {
    drive("tab-popup", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        // Tab 1 is in front; tab 0 (behind) asks for a window: it opens next to tab 0, behind.
        rig.engines.events(
            stage,
            0,
            vec![Event::Popup {
                url: "https://popup.example/".into(),
            }],
            false,
        );
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), 3);
        assert_eq!(made[2].key, "popup.example");
        assert_eq!(visibility(&made[2].engine.calls()), [false]);
        assert_eq!(rig.engines.fronts.borrow().last(), Some(&1));
        assert!(rig.rects.borrow().contains_key("tab_3"));
    });
}

#[test]
fn a_tab_behind_the_front_one_that_stops_shows_the_crashed_page_when_it_comes_to_the_front() {
    drive("tab-crash", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines
            .state(stage, 1, loaded("https://two.example/", "Two", 1), true);
        // Tab 0 loses its view while behind.
        rig.engines.state(
            stage,
            0,
            PageState {
                url: "https://one.example/".into(),
                committed_url: "https://one.example/".into(),
                crashed: true,
                error: "The page's process crashed".into(),
                ..PageState::default()
            },
            false,
        );
        assert!(!rig.rects.borrow().contains_key("hang_bar"));
        stage.inject(chord(Key::DIGIT1, false));
        assert!(
            rig.rects.borrow().contains_key("hang_bar"),
            "the crashed-tab bar is up: {:?}",
            rig.rects.borrow().keys().collect::<Vec<_>>()
        );
        // Coming to the front did not make a new view: only Reload does.
        assert_eq!(rig.engines.made.borrow().len(), 2);
        let (x, y) = rig.centre("hang_reload");
        stage.click(x, y);
        assert!(rig.engines.calls(0).contains(&Call::Reload));
    });
}

#[test]
fn a_dialog_from_a_tab_behind_brings_it_to_the_front_and_the_answer_goes_to_it() {
    drive("tab-dialog", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines.events(
            stage,
            0,
            vec![Event::Dialog {
                id: 5,
                kind: DialogKind::Confirm,
                text: "Leave?".into(),
                default: String::new(),
            }],
            false,
        );
        assert_eq!(
            rig.engines.fronts.borrow().last(),
            Some(&0),
            "the asking tab is in front"
        );
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(0)
            .contains(&Call::DialogReply(5, false, String::new())));
        assert!(!rig
            .engines
            .calls(1)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(..))));
    });
}

#[test]
fn dialogs_with_one_number_in_two_tabs_are_answered_by_their_own() {
    drive("tab-dialog-ids", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        for (tab, front) in [(1, true), (0, false)] {
            rig.engines.events(
                stage,
                tab,
                vec![Event::Dialog {
                    id: 1,
                    kind: DialogKind::Alert,
                    text: "Hi".into(),
                    default: String::new(),
                }],
                front,
            );
        }
        // The second replaced the first (one dialog at a time): the first is refused, in tab 1.
        assert_eq!(
            rig.engines.calls(1).last(),
            Some(&Call::DialogReply(1, false, String::new()))
        );
        stage.inject(key_down(Key::ESCAPE));
        assert!(rig
            .engines
            .calls(0)
            .iter()
            .any(|c| matches!(c, Call::DialogReply(1, ..))));
    });
}

#[test]
fn past_the_limit_the_oldest_used_tabs_give_their_views_up_and_come_back_loading_their_address() {
    drive("tab-discard", None, |stage, rig| {
        first_page(stage, rig);
        for n in 1..20 {
            stage.emit(Msg::Tab(TabMsg::New));
            rig.engines.state(
                stage,
                n,
                loaded(
                    &format!("https://site{n}.example/"),
                    &format!("Site {n}"),
                    1,
                ),
                true,
            );
        }
        let gone = rig.engines.gone.borrow().clone();
        assert_eq!(gone.len(), 4, "20 tabs, 16 views: {gone:?}");
        assert_eq!(
            gone,
            [0, 1, 2, 3],
            "the oldest used first, never the front one"
        );
        assert!(
            rig.rects.borrow().contains_key("tab_20"),
            "they stay in the strip"
        );
        let before = rig.engines.made.borrow().len();
        // Tab 2 (a discarded one) is asked for: a new view loads the address it kept.
        stage.inject(chord(Key::DIGIT2, false));
        let made = rig.engines.made.borrow();
        assert_eq!(made.len(), before + 1);
        let revived = made.last().unwrap();
        assert_eq!(revived.key, "site1.example");
        assert_eq!(loads(&revived.engine.calls()), ["https://site1.example/"]);
    });
}

#[test]
fn a_tab_with_a_dialog_open_is_not_discarded() {
    drive("tab-busy", None, |stage, rig| {
        first_page(stage, rig);
        rig.engines.events(
            stage,
            0,
            vec![Event::Dialog {
                id: 1,
                kind: DialogKind::Alert,
                text: "Wait".into(),
                default: String::new(),
            }],
            true,
        );
        for n in 1..20 {
            stage.emit(Msg::Tab(TabMsg::New));
            rig.engines.state(
                stage,
                n,
                loaded(&format!("https://s{n}.example/"), "S", 1),
                true,
            );
        }
        assert!(
            !rig.engines.gone.borrow().contains(&0),
            "the tab with the dialog stays"
        );
    });
}

#[test]
fn a_download_in_a_tab_behind_keeps_its_view_and_its_cancel_goes_to_that_tab() {
    drive("tab-download", None, |stage, rig| {
        first_page(stage, rig);
        stage.inject(chord(Key::T, false));
        rig.engines.events(
            stage,
            0,
            vec![Event::DownloadStarted {
                id: 7,
                url: "https://one.example/big.zip".into(),
                name: "big.zip".into(),
                total: Some(1000),
            }],
            false,
        );
        for n in 2..20 {
            stage.emit(Msg::Tab(TabMsg::New));
            rig.engines.state(
                stage,
                n,
                loaded(&format!("https://d{n}.example/"), "D", 1),
                true,
            );
        }
        assert!(
            !rig.engines.gone.borrow().contains(&0),
            "a tab with a download running keeps its view"
        );
        assert!(
            rig.engines.gone.borrow().contains(&1),
            "an idle one does not"
        );
        // The download ends: the tab may be discarded again by the next tab.
        rig.engines.events(
            stage,
            0,
            vec![Event::DownloadDone {
                id: 7,
                name: "big.zip".into(),
            }],
            false,
        );
        stage.emit(Msg::Tab(TabMsg::New));
        assert!(rig.engines.gone.borrow().contains(&0));
    });
}

fn saved_three(home: &Path) {
    fs::write(home.join(session::RUNNING), "x").unwrap();
    let tab = |url: &str, title: &str| Tab {
        url: url.into(),
        title: title.into(),
    };
    let mut window = Window::new(
        vec![
            tab("https://a.example/", "A"),
            tab("https://b.example/", "B"),
            tab("https://c.example/", "C"),
        ],
        1,
    );
    window.closed = vec![tab("https://old.example/", "Old")];
    session::save(
        home,
        &Session {
            windows: vec![window],
        },
    )
    .unwrap();
}

fn always(home: &Path, host: Host) -> Host {
    Host {
        profile: Some(home.to_path_buf()),
        prefs: Prefs {
            restore: Restore::Always,
            ..Prefs::default()
        },
        ..host
    }
}

#[test]
fn a_restored_session_loads_only_the_front_tab_and_the_others_load_when_asked() {
    let (image, _, _) = drive_with(
        "tab-restore",
        None,
        saved_three,
        |host, home| always(home, host),
        |stage, rig| {
            {
                let made = rig.engines.made.borrow();
                assert_eq!(made.len(), 1, "only the active tab has a view");
                assert_eq!(loads(&made[0].engine.calls()), ["https://b.example/"]);
                assert_eq!(made[0].key, "b.example");
            }
            assert!(
                rig.rects.borrow().contains_key("tab_3"),
                "all three are in the strip"
            );
            // Tab 1 loads when it is asked for, and the closed tab is still there to reopen.
            stage.inject(chord(Key::DIGIT1, false));
            assert_eq!(
                loads(&rig.engines.made.borrow()[1].engine.calls()),
                ["https://a.example/"]
            );
            stage.inject(chord(Key::T, true));
            assert_eq!(
                loads(&rig.engines.made.borrow()[2].engine.calls()),
                ["https://old.example/"]
            );
        },
    );
    save(&image, "lazyweb-tabs-restore.png");
}

#[test]
fn the_tabs_are_written_whole_with_the_closed_ones_and_only_web_pages() {
    let dir = std::env::temp_dir().join(format!("lazyweb-win-{}-tab-session", std::process::id()));
    drive("tab-session", Some("https://one.example/"), |stage, rig| {
        rig.state(stage, loaded("https://one.example/", "One", 1));
        stage.inject(chord(Key::T, false));
        rig.engines
            .state(stage, 1, loaded("https://two.example/", "Two", 1), true);
        stage.emit(Msg::Tab(TabMsg::New));
        rig.engines.state(
            stage,
            2,
            loaded("data:text/html,history", "History", 1),
            true,
        );
        stage.inject(chord(Key::W, false));
        // At most one write a second: the newest session goes out on the beat after that.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        stage.emit(Msg::Engine);
        // Written on the disk thread; the window's quit waits for it.
        assert!(xui_lazyweb::disk::global().flush(std::time::Duration::from_secs(5)));
    });
    let kept = session::load(&dir).expect("a session was written");
    let window = &kept.windows[0];
    let urls: Vec<_> = window.tabs.iter().map(|t| t.url.as_str()).collect();
    assert_eq!(urls, ["https://one.example/", "https://two.example/"]);
    assert!(
        window.closed.is_empty(),
        "the history page was not worth keeping"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn the_view_menu_agent_is_told_to_tabs_made_later() {
    drive("tab-agent", None, |stage, rig| {
        first_page(stage, rig);
        stage.emit(Msg::UserAgent(Some(2)));
        stage.inject(chord(Key::T, false));
        assert!(rig
            .engines
            .calls(1)
            .iter()
            .any(|c| matches!(c, Call::SetUserAgent(agent) if !agent.is_empty())));
    });
}

#[test]
fn the_strip_with_a_loading_a_discarded_and_a_crashed_tab_is_drawn() {
    let (image, _) = drive("tab-picture", None, |stage, rig| {
        rig.state(
            stage,
            loaded("https://one.example/", "A page with a rather long title", 1),
        );
        for n in 1..4 {
            stage.emit(Msg::Tab(TabMsg::New));
            rig.engines.state(
                stage,
                n,
                loaded(
                    &format!("https://site{n}.example/"),
                    &format!("Site {n}"),
                    1,
                ),
                true,
            );
        }
        // One loads, one stops.
        rig.engines
            .state(stage, 3, loading("https://site3.example/next"), true);
        rig.engines.state(
            stage,
            2,
            PageState {
                url: "https://site2.example/".into(),
                crashed: true,
                error: "The page's process crashed".into(),
                ..PageState::default()
            },
            false,
        );
    });
    save(&image, "lazyweb-tabs-strip.png");
}
