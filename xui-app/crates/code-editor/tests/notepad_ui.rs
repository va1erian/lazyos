//! Editor-level behaviour the pure `search` module cannot cover on its own, and
//! a headless light/dark render of the notepad's Editor plus a find bar.
//!
//! The offscreen backend runs to completion synchronously, so these tests do not
//! enter a live message loop.

use std::path::PathBuf;
use std::rc::Rc;

use xui_canvas::OffscreenBackend;
use xui_canvas::snapshot::{Snapshot, render};
use xui_code_editor::find::Query;
use xui_code_editor::search::{self, SearchState};
use xui_code_editor::{Editor, FontConfig, Options};
use xui_core::app::{App, Ui, run_app};
use xui_core::backend::PlatformSpec;
use xui_core::geometry::Rect;
use xui_core::units::Dip;
use xui_core::widget::{Button, CheckBox, Edit, Label};
use xui_core::{Theme, dip};

/// A plain app for the offscreen editor tests.
struct Empty;

impl App for Empty {
    type Msg = ();
    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

fn with_editor(check: impl FnOnce(&Editor<()>) + 'static) {
    let check = std::cell::RefCell::new(Some(check));
    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("notepad").size(dip(300.0), dip(200.0)),
        move |ui| {
            let editor = Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
            (check.borrow_mut().take().expect("check"))(&editor);
            Empty
        },
    )
    .expect("run_app");
}

#[test]
fn replace_all_replaces_every_match() {
    with_editor(|editor| {
        editor.set_text("one two one");
        let state = SearchState {
            query_text: "one".to_string(),
            ..SearchState::default()
        };
        let text = editor.text();
        let replaced = search::replace_all(&text, &state, "X")
            .expect("valid")
            .expect("something changed");
        assert_eq!(replaced, "X two X");
    });
}

#[test]
fn replace_all_through_the_editor_is_one_undo_step() {
    with_editor(|editor| {
        editor.set_text("one two one");
        let state = SearchState {
            query_text: "one".to_string(),
            ..SearchState::default()
        };
        let text = editor.text();
        let replaced = search::replace_all(&text, &state, "X")
            .expect("valid")
            .expect("something changed");
        let chars = text.chars().count();
        assert!(editor.replace(0, chars, &replaced));
        assert_eq!(editor.text(), "X two X");
        assert!(editor.undo(), "one undo restores the original");
        assert_eq!(editor.text(), "one two one");
    });
}

#[test]
fn replace_current_only_replaces_when_the_selection_is_a_match() {
    with_editor(|editor| {
        editor.set_text("one two one");
        let state = SearchState {
            query_text: "one".to_string(),
            ..SearchState::default()
        };
        editor.select(4, 7); // "two", not a match
        assert_eq!(
            search::replacement_for(&editor.text(), &state, editor.selection(), "X")
                .expect("valid"),
            None
        );
        editor.select(0, 3); // "one", a match
        assert_eq!(
            search::replacement_for(&editor.text(), &state, editor.selection(), "X")
                .expect("valid"),
            Some("X".to_string())
        );
    });
}

#[test]
fn set_text_replaces_the_buffer_and_clears_the_undo_history() {
    with_editor(|editor| {
        editor.set_text("first");
        assert!(editor.insert_text("!"));
        assert!(editor.can_undo());
        editor.set_text("second");
        assert!(!editor.can_undo(), "loading clears undo, not an edit");
        assert!(!editor.can_redo());
        assert_eq!(editor.text(), "second");
        assert_eq!(editor.caret(), 0, "the caret moves to the top");
    });
}

#[test]
fn find_next_wraps_around_the_buffer() {
    with_editor(|editor| {
        editor.set_text("ab ab");
        let query = Query::literal("ab");
        assert!(editor.find_next(&query, true, true).expect("valid"));
        assert_eq!(editor.selection(), Some((0, 2)));
        assert!(editor.find_next(&query, true, true).expect("valid"));
        assert_eq!(editor.selection(), Some((3, 5)));
        assert!(editor.find_next(&query, true, true).expect("valid"));
        assert_eq!(editor.selection(), Some((0, 2)), "wrapped to the top");
    });
}

/// The rendered app: the editor with a find/replace bar above it.
struct NotepadApp {
    _editor: Editor<()>,
    _query: Edit<()>,
    _replacement: Edit<()>,
    _status: Label<()>,
    _next: Button<()>,
    _replace: Button<()>,
    _case: CheckBox<()>,
}

impl App for NotepadApp {
    type Msg = ();
    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

/// Renders the editor plus find bar in one theme.
fn render_notepad(theme: Theme) -> xui_core::Image {
    render(Snapshot::new(Dip(720.0), Dip(320.0)).theme(theme), |ui| {
        let dpi = ui.dpi();
        let p = |value: f32| dip(value).to_px(dpi).value();
        let options = Options {
            font: FontConfig {
                family: Some("monospace".to_owned()),
                ..FontConfig::default()
            },
            ..Options::default()
        };
        let editor =
            Editor::with_options(ui, Rect::new(p(0.0), p(44.0), p(720.0), p(320.0)), options)
                .expect("editor");
        editor.set_text("fn main() {\n    let answer = 42;\n}\n");
        NotepadApp {
            _editor: editor,
            _query: Edit::new(ui, Rect::new(p(8.0), p(8.0), p(200.0), p(40.0)), "answer")
                .expect("query"),
            _replacement: Edit::new(ui, Rect::new(p(206.0), p(8.0), p(380.0), p(40.0)), "x")
                .expect("replacement"),
            _status: Label::new(ui, Rect::new(p(386.0), p(8.0), p(460.0), p(40.0)), "1 of 1")
                .expect("status"),
            _next: Button::new(ui, Rect::new(p(466.0), p(8.0), p(530.0), p(40.0)), "Next")
                .expect("next"),
            _replace: Button::new(
                ui,
                Rect::new(p(536.0), p(8.0), p(620.0), p(40.0)),
                "Replace all",
            )
            .expect("replace"),
            _case: CheckBox::new(
                ui,
                Rect::new(p(626.0), p(8.0), p(716.0), p(40.0)),
                "Match case",
            )
            .expect("case"),
        }
    })
    .expect("render")
}

#[test]
fn the_notepad_layout_renders_in_light_and_dark() {
    let light = render_notepad(Theme::light());
    let dark = render_notepad(Theme::dark());
    assert!(light.width() > 0 && light.height() > 0);
    assert!(dark.width() > 0 && dark.height() > 0);
    assert_ne!(light, dark, "the two themes render differently");

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    light
        .save_png(dir.join("notepad-light.png"))
        .expect("save light");
    dark.save_png(dir.join("notepad-dark.png"))
        .expect("save dark");
}
