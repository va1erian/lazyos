//! The suggestions under the address field, driven offscreen against a recording engine: typing
//! ranks the history, the arrows and Enter open a row, Shift+Delete forgets a page for good.
//! Snapshots go to `xui-app/target/snapshots/lazyweb-suggest*.png`.

mod support;

use support::*;
use xui_canvas::snapshot::Stage;
use xui_core::backend::Event as UiEvent;
use xui_webkit::engine::Call;
use xui_lazyweb::app::Msg;
use xui_lazyweb::suggest::dropdown::Act;
use xui_lazyweb::visits::Visits;

/// Visits three pages, so the history has something to suggest.
fn browse(stage: &Stage<'_, Msg>, rig: &Rig) {
    let pages = [
        ("http://example.com/", "Example Domain"),
        ("http://example.org/docs", "Example docs"),
        ("http://other.test/", "Something else"),
    ];
    for (n, (url, title)) in pages.into_iter().enumerate() {
        rig.state(stage, loaded(url, title, n as u32 + 1));
    }
    rig.engine.take();
}

fn type_text(stage: &Stage<'_, Msg>, text: &str) {
    stage.emit(Msg::FocusAddress);
    for c in text.chars() {
        stage.inject(UiEvent::Char(c));
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

#[test]
fn typing_ranks_the_history_and_enter_opens_the_row_the_arrows_chose() {
    let (image, calls) = drive("suggest-open", Some("about:blank"), |stage, rig| {
        browse(stage, rig);
        type_text(stage, "exam");
        stage.emit(Msg::Suggest(Act::Move(1)));
        stage.emit(Msg::Suggest(Act::Move(1)));
    });
    // Two pages match "exam"; the dropdown is up in the picture, over the page.
    save(&image, "lazyweb-suggest.png");
    assert!(
        loads(&calls).is_empty(),
        "nothing opens by itself: {calls:?}"
    );

    let (_, calls) = drive("suggest-enter", Some("about:blank"), |stage, rig| {
        browse(stage, rig);
        type_text(stage, "exam");
        stage.emit(Msg::Suggest(Act::Move(1)));
        stage.emit(Msg::Suggest(Act::Move(1)));
        stage.emit(Msg::Go);
    });
    assert_eq!(loads(&calls), ["http://example.org/docs"]);
}

#[test]
fn enter_without_moving_opens_what_was_typed_and_the_search_row_searches() {
    let (_, calls) = drive("suggest-typed", Some("about:blank"), |stage, rig| {
        browse(stage, rig);
        type_text(stage, "other.test");
        stage.emit(Msg::Go);
        type_text(stage, "exam");
        // The last row is the search for the words.
        for _ in 0..3 {
            stage.emit(Msg::Suggest(Act::Move(1)));
        }
        stage.emit(Msg::Go);
    });
    assert_eq!(
        loads(&calls),
        ["https://other.test/", "https://duckduckgo.com/?q=exam"]
    );
}

#[test]
fn escape_puts_the_dropdown_away_and_a_click_on_a_row_opens_it() {
    let (_, calls) = drive("suggest-click", Some("about:blank"), |stage, rig| {
        browse(stage, rig);
        type_text(stage, "oth");
        stage.emit(Msg::Suggest(Act::Close));
        stage.emit(Msg::Go);
        type_text(stage, "oth");
        stage.emit(Msg::Suggest(Act::Pick(0)));
    });
    // The first Go opens the typed words as a search (the dropdown was gone); the pick opens a page.
    assert_eq!(
        loads(&calls),
        ["https://duckduckgo.com/?q=oth", "http://other.test/"]
    );
}

#[test]
fn shift_delete_forgets_the_page_for_good_and_the_file_is_rewritten() {
    let (_, _calls) = drive("suggest-remove", Some("about:blank"), |stage, rig| {
        browse(stage, rig);
        type_text(stage, "exam");
        stage.emit(Msg::Suggest(Act::Move(1)));
        stage.emit(Msg::Suggest(Act::Remove));
    });
    // The rig keeps its files in a folder named for the test (`support::scratch`).
    let file = std::env::temp_dir()
        .join(format!(
            "lazyweb-win-{}-suggest-remove",
            std::process::id()
        ))
        .join("history.tsv");
    let again = Visits::open(file);
    let urls: Vec<_> = again.entries().iter().map(|v| v.url.as_str()).collect();
    // The top match for "exam" is the page seen most recently of the two.
    assert!(urls.contains(&"http://other.test/"));
    assert_eq!(urls.len(), 2, "{urls:?}");
}
