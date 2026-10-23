//! The star, Ctrl+D, the editor and the Bookmarks menu, driven offscreen against a recording
//! engine; what was kept is read back from `bookmarks.log`. Snapshots go to
//! `xui-app/target/snapshots/lazyweb-bookmarks-*.png`.

mod support;

use support::*;

use xui_core::backend::Event as UiEvent;
use xui_core::{Key, Modifiers, MouseButton};
use xui_webkit::engine::{Button, Call};
use xui_webkit::events::Event;
use xui_lazyweb::app::Msg;
use xui_lazyweb::bookmarks::BookmarkMsg;
use xui_lazyweb::chrome::Command;
use xui_lazyweb::places::{Bookmarks, Kind, UNSORTED};

fn stored(rig: &Rig) -> Bookmarks {
    Bookmarks::open(rig.bookmarks_file())
}

#[test]
fn the_star_bookmarks_the_page_into_unsorted_and_a_second_click_does_not_add_again() {
    let (image, _) = drive("star", Some("https://example.com/a"), |stage, rig| {
        rig.state(stage, loaded("https://example.com/a", "Example A", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        let set = stored(rig);
        assert_eq!(set.len(), 1);
        let e = set.iter().next().unwrap();
        assert_eq!(
            (e.parent, e.kind, e.title.as_str(), e.url.as_str()),
            (UNSORTED, Kind::Link, "Example A", "https://example.com/a")
        );
        // The second click opens the editor: nothing new is kept.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        assert_eq!(stored(rig).len(), 1);
    });
    save(&image, "lazyweb-bookmarks-editor.png");
}

#[test]
fn ctrl_d_is_the_star_and_the_page_does_not_see_it() {
    let (_, calls) = drive("ctrl_d", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        let page = page_origin(stage);
        stage.click(page.0 + 40, page.1 + 40);
        rig.engine.take();
        stage.inject(UiEvent::KeyDown {
            key: Key::D,
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
            repeat: 1,
            system: false,
        });
        assert_eq!(stored(rig).len(), 1);
    });
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Key(_, _, _, _))),
        "{calls:?}"
    );
}

#[test]
fn the_editor_renames_refiles_and_removes() {
    let (_, _) = drive("editor", Some("https://example.com/"), |stage, rig| {
        rig.state(stage, loaded("https://example.com/", "Example", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        // Done with the name and folder as they were: the same bookmark, saved.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Done));
        let set = stored(rig);
        assert_eq!(set.len(), 1);
        assert_eq!(set.iter().next().unwrap().parent, UNSORTED);

        // Open it again and remove it.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Remove));
        assert!(stored(rig).is_empty());
        // Remove with no editor open does nothing.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Remove));

        // Bookmarked again, then Cancel keeps it.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Cancel));
        assert_eq!(stored(rig).len(), 1);
    });
}

#[test]
fn a_browser_page_or_a_data_page_is_never_bookmarked() {
    let (_, _) = drive("never", None, |stage, rig| {
        // The start page is a `data:` page the browser built.
        stage.emit(Msg::Menu(Command::ShowHistory));
        let url = rig.internal.url_of(xui_lazyweb::pages::HISTORY);
        rig.state(stage, loaded(&url, "History", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Menu(Command::BookmarkPage));
        rig.state(stage, loaded("javascript:alert(1)", "x", 2));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        assert!(!rig.bookmarks_file().exists() || stored(rig).is_empty());
    });
}

#[test]
fn the_bookmarks_menu_commands_bookmark_edit_and_list() {
    let (image, _) = drive("menu", Some("https://example.com/"), |stage, rig| {
        rig.state(stage, loaded("https://example.com/", "Example", 1));
        stage.emit(Msg::Menu(Command::BookmarkPage));
        assert_eq!(stored(rig).len(), 1);
        stage.emit(Msg::Menu(Command::EditBookmark));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Cancel));
        stage.emit(Msg::Menu(Command::ShowBookmarks));
    });
    save(&image, "lazyweb-bookmarks-list.png");
}

#[test]
fn picking_a_row_of_the_list_opens_that_bookmark() {
    let (_, calls) = drive("pick", Some("https://example.com/"), |stage, rig| {
        rig.state(stage, loaded("https://example.com/", "Example", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        // Leave the page, show the list, choose its only row.
        rig.state(stage, loaded("https://other.test/", "Other", 2));
        rig.engine.take();
        stage.emit(Msg::Menu(Command::ShowBookmarks));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Pick(0)));
        // A row that is not there does nothing.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Pick(99)));
    });
    let loads: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(u) => Some(u.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(loads.last(), Some(&"https://example.com/"), "{loads:?}");
}

/// A right press and release at a point of the window.
fn right(stage: &xui_canvas::snapshot::Stage<'_, Msg>, x: i32, y: i32) {
    for event in [
        UiEvent::MouseDown {
            x,
            y,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        },
        UiEvent::MouseUp {
            x,
            y,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

#[test]
fn a_right_click_on_a_link_belongs_to_the_window_and_one_on_the_page_goes_on_to_the_page() {
    let rights = |calls: &[Call]| {
        calls
            .iter()
            .filter(|c| matches!(c, Call::Mouse(_, Button::Right, _, _, _)))
            .count()
    };
    let (_, on_link) = drive("rc_link", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
        let (ox, oy) = page_origin(stage);
        rig.engine.take();
        right(stage, ox + 60, oy + 40);
    });
    assert_eq!(rights(&on_link), 0, "{on_link:?}");

    for (name, hover) in [
        ("rc_none", ""),
        ("rc_script", "javascript:void(0)"),
        ("rc_cmd", "x-lazyweb:clear-history"),
    ] {
        let (_, calls) = drive(name, Some("http://example.com/"), move |stage, rig| {
            rig.state(stage, loaded("http://example.com/", "Example", 1));
            rig.events(stage, vec![Event::Hover(hover.into())]);
            let (ox, oy) = page_origin(stage);
            rig.engine.take();
            right(stage, ox + 60, oy + 40);
        });
        // Press and release both reach the page.
        assert_eq!(rights(&calls), 2, "{name}: {calls:?}");
    }
}

#[test]
fn a_link_for_another_app_is_handed_over_only_from_the_menu() {
    let (_, _, opened) = drive_full("rc_open", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("mailto:a@b.test".into())]);
        let (ox, oy) = page_origin(stage);
        stage.inject(UiEvent::MouseDown {
            x: ox + 60,
            y: oy + 40,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        });
        stage.emit(Msg::Menu(Command::OpenLinkInApp));
    });
    assert_eq!(*opened.urls.borrow(), ["mailto:a@b.test"]);

    // A web link: the menu item is disabled, and even sent, the command hands nothing over.
    let (_, _, opened) = drive_full("rc_web", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
        let (ox, oy) = page_origin(stage);
        stage.inject(UiEvent::MouseDown {
            x: ox + 60,
            y: oy + 40,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        });
        stage.emit(Msg::Menu(Command::OpenLinkInApp));
    });
    assert!(opened.urls.borrow().is_empty());
}
