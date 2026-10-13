//! The LazyWebPlus window driven offscreen against a recording engine: the engine's news reaches the
//! chrome, and the pointer, wheel and keyboard reach the engine. Snapshots go to
//! `xui-app/target/snapshots/lazywebplus-*.png` for a human to look at.

mod support;

use std::fs;
use support::*;

use xui_core::Key;
use xui_webkit::engine::Call;
use xui_webkit::events::{CursorKind, Event};
use xui_webkit::mailbox::PageState;
use xui_webplus::app::Msg;
use xui_webplus::chrome::Command;
use xui_webplus::pages::ABOUT;

#[test]
fn the_first_address_is_loaded_and_the_page_is_laid_out_at_the_view_size() {
    let (_, calls) = drive("first", Some("http://example.com/"), |_, _| {});
    assert!(
        calls.contains(&Call::Load("http://example.com/".into())),
        "{calls:?}"
    );
    let (w, h) = calls
        .iter()
        .rev()
        .find_map(|c| match c {
            Call::Resize(w, h) => Some((*w, *h)),
            _ => None,
        })
        .expect("the engine was told the view size");
    assert_eq!(w, SIZE.0);
    assert!(h > 300 && h < SIZE.1, "view height {h}");
}

#[test]
fn without_an_address_the_start_page_opens() {
    let (_, calls) = drive("start", None, |_, _| {});
    let loads: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(loads.len(), 1);
    assert!(loads[0].starts_with("data:text/html"), "{loads:?}");
}

#[test]
fn a_frame_from_the_engine_is_drawn_and_the_chrome_follows_the_page() {
    let (image, _) = drive("frame", Some("https://theoldnet.com/"), |stage, rig| {
        rig.mailbox.post_frame(880, 400, &test_page(880, 400));
        stage.emit(Msg::Frame);
        rig.state(stage, loaded("https://theoldnet.com/", "TheOldNet", 1));
    });
    save(&image, "lazywebplus-frame.png");
    let reds = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
        .count();
    assert!(reds > 5_000, "the red block is on screen: {reds}");
}

#[test]
fn a_finished_load_is_written_into_the_history_once_and_its_late_title_updates_it() {
    let (_, _) = drive("visits", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loading("http://example.com/"));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        // The same state again (the engine posts several): no second visit.
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.state(stage, loaded("http://example.com/", "Example Domain", 1));
        let file = fs::read_to_string(rig.history_file()).unwrap();
        let lines: Vec<_> = file.lines().collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].ends_with("\thttp://example.com/\tExample Domain"));
    });
}

#[test]
fn a_stopped_a_failed_and_a_download_load_leave_no_history() {
    let (_, _) = drive("nohistory", None, |stage, rig| {
        // Loading ends with no finished navigation: stopped, or taken over by a download.
        rig.state(stage, loading("http://a.test/"));
        rig.state(stage, PageState::default());
        // A load that failed.
        rig.state(stage, loading("http://b.test/"));
        rig.state(
            stage,
            PageState {
                error: "Could not load http://b.test/: Connection refused".into(),
                ..PageState::default()
            },
        );
        assert!(!rig.history_file().exists());
    });
}

#[test]
fn the_link_under_the_pointer_shows_in_the_status_bar_and_goes_away() {
    let (with_link, _) = drive("hover", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
    });
    save(&with_link, "lazywebplus-hover.png");
    let (without, _) = drive("hover2", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
        rig.events(stage, vec![Event::Hover(String::new())]);
    });
    let (never, _) = drive("hover3", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
    });
    assert_ne!(with_link.pixels(), never.pixels(), "the link is drawn");
    assert_eq!(without.pixels(), never.pixels(), "and removed");
}

#[test]
fn the_browsers_own_pages_are_built_and_loaded_by_name() {
    let (_, calls) = drive("pages", None, |stage, _| {
        stage.emit(Msg::Menu(Command::ShowHistory));
        stage.emit(Msg::Menu(Command::About));
    });
    let loads: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(loads.len(), 3, "{loads:?}");
    assert!(loads[1].contains("History") && loads[1].starts_with("data:"));
    assert!(loads[2].contains("About%20LazyWeb") || loads[2].contains("About LazyWeb"));
}

#[test]
fn escape_stops_a_load_but_belongs_to_the_page_otherwise() {
    let (_, calls) = drive("escape", Some("http://example.com/"), |stage, rig| {
        let (ox, oy) = page_origin(stage);
        stage.click(ox + 50, oy + 40);
        rig.state(stage, loading("http://example.com/"));
        rig.engine.take();
        stage.inject(key_down(Key::ESCAPE));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        stage.inject(key_down(Key::ESCAPE));
    });
    let stops = calls.iter().filter(|c| matches!(c, Call::Stop)).count();
    assert_eq!(stops, 1, "{calls:?}");
    let escapes_to_page = calls
        .iter()
        .filter(|c| matches!(c, Call::Key(true, 0xff1b, _, _)))
        .count();
    assert_eq!(
        escapes_to_page, 1,
        "the idle Escape reached the page: {calls:?}"
    );
}

#[test]
fn reload_of_one_of_our_pages_builds_it_again() {
    let (_, calls) = drive("reload", None, |stage, rig| {
        stage.emit(Msg::Menu(Command::About));
        let url = rig.internal.url_of(ABOUT);
        rig.state(stage, loaded(&url, "About LazyWeb+", 1));
        rig.engine.take();
        stage.emit(Msg::Menu(Command::Reload));
    });
    assert!(calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.starts_with("data:"))));
    assert!(!calls.contains(&Call::Reload));
}

#[test]
fn a_right_click_on_a_link_offers_to_copy_it_and_the_pointer_shape_follows_the_page() {
    let (image, _) = drive("context", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(
            stage,
            vec![
                Event::Cursor(CursorKind::Hand),
                Event::Hover("http://example.com/more".into()),
            ],
        );
        stage.emit(Msg::PageMenu(60, 40));
    });
    save(&image, "lazywebplus-context.png");
}
