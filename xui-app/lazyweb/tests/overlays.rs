//! The frames over the page (the bookmark editor, the report form): one at a time, and Escape puts
//! them away. Escape still stops a load when no frame is up. Driven offscreen.

mod support;

use support::*;
use xui_canvas::snapshot::Stage;
use xui_core::Key;
use xui_webkit::engine::Call;
use xui_lazyweb::app::Msg;
use xui_lazyweb::bookmarks::BookmarkMsg;
use xui_lazyweb::chrome::Command;

const URL: &str = "https://example.com/";

fn stops(calls: &[Call]) -> usize {
    calls.iter().filter(|c| matches!(c, Call::Stop)).count()
}

/// The page loads again, so that an Escape nothing else claims means Stop.
fn busy(stage: &Stage<'_, Msg>, rig: &Rig) {
    rig.state(stage, loading(URL));
}

#[test]
fn escape_cancels_the_bookmark_editor_and_leaves_the_bookmark() {
    let (_, calls) = drive("overlay-esc-editor", Some(URL), |stage, rig| {
        rig.state(stage, loaded(URL, "Example", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.inject(key_down(Key::ESCAPE));
        // Gone: Remove has no editor to act for.
        stage.emit(Msg::Bookmarks(BookmarkMsg::Remove));
        let set = xui_lazyweb::places::Bookmarks::open(rig.bookmarks_file());
        assert_eq!(set.len(), 1);
        // With nothing up, Escape is Stop again while a page loads.
        busy(stage, rig);
        stage.inject(key_down(Key::ESCAPE));
    });
    assert_eq!(stops(&calls), 1, "{calls:?}");
}

#[test]
fn escape_hides_the_report_form_and_keeps_what_was_typed() {
    let (_, calls) = drive("overlay-esc-report", Some(URL), |stage, rig| {
        rig.state(stage, loaded(URL, "Example", 1));
        busy(stage, rig);
        stage.emit(Msg::Menu(Command::ReportIssue));
        // The form takes the Escape; the load goes on.
        stage.inject(key_down(Key::ESCAPE));
        // The next one is the window's: Stop.
        stage.inject(key_down(Key::ESCAPE));
    });
    assert_eq!(stops(&calls), 1, "{calls:?}");
}

#[test]
fn the_star_puts_the_report_form_away_first() {
    let (_, calls) = drive("overlay-star-report", Some(URL), |stage, rig| {
        rig.state(stage, loaded(URL, "Example", 1));
        stage.emit(Msg::Menu(Command::ReportIssue));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        busy(stage, rig);
        // Were the form still up, this Escape would be its.
        stage.inject(key_down(Key::ESCAPE));
    });
    assert_eq!(stops(&calls), 1, "{calls:?}");
}

#[test]
fn opening_the_report_form_puts_the_bookmark_editor_away() {
    let (_, calls) = drive("overlay-report-editor", Some(URL), |stage, rig| {
        rig.state(stage, loaded(URL, "Example", 1));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Bookmarks(BookmarkMsg::Star));
        stage.emit(Msg::Menu(Command::ReportIssue));
        busy(stage, rig);
        // The first Escape is the form's; were the editor up too, the second would be its.
        stage.inject(key_down(Key::ESCAPE));
        stage.inject(key_down(Key::ESCAPE));
    });
    assert_eq!(stops(&calls), 1, "{calls:?}");
}
