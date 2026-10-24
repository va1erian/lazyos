//! Tabs in the window: the session, the restore and the drawn strip.

mod support;

use std::fs;
use std::path::Path;

use support::*;
use xui_core::Key;
use xui_lazyweb::app::{Host, Msg};
use xui_lazyweb::prefs::{Prefs, Restore};
use xui_lazyweb::session::{self, Session, Tab, Window};
use xui_lazyweb::strip::TabMsg;
use xui_webkit::engine::Call;
use xui_webkit::mailbox::PageState;

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
