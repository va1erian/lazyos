//! Tests for the [`Editor`](super::Editor) widget: clipboard installation,
//! command-state queries, hit-testing off the node origin, programmatic edits
//! and find, driven through an offscreen backend.

#[test]
fn the_default_clipboard_is_the_windows_portable_one() {
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::app::{App, Ui, run_app};
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;

    struct Empty;
    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("clipboard").size(Dip(200.0), Dip(100.0)),
        |ui| {
            let editor = super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100)).expect("editor");
            editor.set_text("hello");
            editor.select_all();
            assert!(editor.copy());
            assert_eq!(ui.clipboard_text().as_deref(), Some("hello"));
            ui.set_clipboard_text("world");
            assert!(editor.paste());
            assert_eq!(editor.text(), "world");
            Empty
        },
    )
    .expect("run_app");
}

#[test]
fn command_state_queries_follow_the_buffer_and_clipboard() {
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::app::{App, Ui, run_app};
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;

    use crate::Clipboard;
    use crate::platform::InProcessClipboard;

    struct Empty;
    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("queries").size(Dip(200.0), Dip(100.0)),
        |ui| {
            let editor = super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100)).expect("editor");
            editor.set_clipboard(InProcessClipboard);
            InProcessClipboard.set_text("");
            assert!(editor.is_empty());
            assert!(!editor.can_undo() && !editor.can_redo() && !editor.can_paste());
            assert!(!editor.insert_text(""), "an empty insert is a no-op");
            assert!(!editor.can_undo());
            assert!(editor.insert_text("hi"));
            assert!(!editor.is_empty() && editor.can_undo());
            assert!(editor.undo());
            assert!(editor.can_redo());
            InProcessClipboard.set_text("x");
            assert!(editor.can_paste());
            Empty
        },
    )
    .expect("run_app");
}

#[test]
fn with_clipboard_installs_the_given_clipboard() {
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::app::{App, Ui, run_app};
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;

    use crate::Clipboard;

    struct Host;
    impl Clipboard for Host {
        fn text(&self) -> Option<String> {
            Some("from the host".to_owned())
        }
        fn set_text(&self, _text: &str) {}
    }

    struct Empty;
    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("clipboard").size(Dip(200.0), Dip(100.0)),
        |ui| {
            let editor = super::Editor::<()>::new(ui, Rect::new(0, 0, 200, 100))
                .expect("editor")
                .with_clipboard(Host);
            assert_eq!(
                editor.state.borrow().clipboard.text().as_deref(),
                Some("from the host")
            );
            Empty
        },
    )
    .expect("run_app");
}

use crate::markers::{Marker, MarkerKind};
use crate::options::Options;
use crate::state::EditorState;

#[test]
fn default_options_are_monospace_friendly() {
    let options = Options::default();
    assert_eq!(options.tab_width, 4);
    assert!(options.show_gutter);
}

#[test]
fn a_marker_can_be_attached_to_a_diagnostic_span() {
    let marker = Marker::new(4, 2, 9, MarkerKind::Error);
    assert!(marker.has_span());
}

#[test]
fn the_state_exposes_selection_over_the_buffer() {
    let state = EditorState::new("abc\ndef", Options::default(), Box::new(SafeClipboard));
    assert_eq!(state.buffer.line_count(), 2);
}

#[test]
fn the_widget_builds_and_paints_through_an_offscreen_backend() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::{App, Image, run_app};

    struct Empty;

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    let backend = Rc::new(OffscreenBackend::new());
    let shot: Rc<RefCell<Option<Image>>> = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&shot);
    run_app(
        backend,
        PlatformSpec::new("editor").size(Dip(200.0), Dip(120.0)),
        move |ui| {
            let editor = crate::Editor::new(ui, Rect::new(0, 0, 200, 120)).expect("editor");
            editor.set_text("hello\nworld");
            *sink.borrow_mut() = ui.capture().ok();
            Empty
        },
    )
    .expect("run_app");

    let captured = shot.borrow();
    let image = captured.as_ref().expect("the editor painted something");
    assert!(image.width() > 0 && image.height() > 0);
}

/// A click lands on the line and column under it wherever the editor sits.
///
/// Mouse events arrive in node-local coordinates, so hit-testing must not
/// use the editor's rectangle relative to its parent: an editor offset
/// inside a panel (a code tab below its header) would map every click to
/// the wrong cell.
#[test]
fn a_click_inside_an_offset_editor_puts_the_caret_under_the_pointer() {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use xui_canvas::snapshot::{Snapshot, render_with};
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::widget::Panel;
    use xui_core::{App, Color};

    use crate::metrics::{CELL_PROBE, Metrics};

    struct Empty(#[allow(dead_code)] Panel<()>);

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    let text: String = (0..12)
        .map(|n| {
            format!(
                "line {n:02} xxxxxxxxxxxxxxxx
"
            )
        })
        .collect();
    let (line, col) = (4, 7);
    // The editor sits at (60, 50) inside a panel that sits at (30, 20).
    let panel_origin = (30, 20);
    let editor_origin = (60, 50);

    let editor: Rc<RefCell<Option<crate::Editor<()>>>> = Rc::new(RefCell::new(None));
    let target = Rc::new(Cell::new((0, 0)));
    let expected = Rc::new(Cell::new(0));
    let result = Rc::new(Cell::new(usize::MAX));
    render_with(
        Snapshot::new(Dip(500.0), Dip(400.0)),
        {
            let editor = Rc::clone(&editor);
            let target = Rc::clone(&target);
            let expected = Rc::clone(&expected);
            let text = text.clone();
            move |ui| {
                let panel = Panel::new(ui, Rect::new(panel_origin.0, panel_origin.1, 480, 380))?;
                let scoped = ui.with_parent(panel.id());
                let widget = crate::Editor::new(
                    &scoped,
                    Rect::new(editor_origin.0, editor_origin.1, 400, 300),
                )?;
                widget.set_text(&text);

                let options = crate::Options::default();
                let style = options.font.style(Color::rgb(0, 0, 0));
                let measured = ui.measure_text(CELL_PROBE, &style, ui.dpi());
                let metrics = Metrics::new(
                    measured,
                    widget.state.borrow().buffer.line_count(),
                    options.show_gutter,
                    ui.dpi(),
                );
                // The middle of the cell, in window coordinates.
                target.set((
                    panel_origin.0
                        + editor_origin.0
                        + metrics.gutter
                        + col as i32 * metrics.advance
                        + metrics.advance / 2,
                    panel_origin.1
                        + editor_origin.1
                        + line as i32 * metrics.line_height
                        + metrics.line_height / 2,
                ));
                expected.set(widget.state.borrow().buffer.line_start(line) + col);
                *editor.borrow_mut() = Some(widget);
                Ok(Empty(panel))
            }
        },
        {
            let editor = Rc::clone(&editor);
            let target = Rc::clone(&target);
            let result = Rc::clone(&result);
            move |stage| {
                let (x, y) = target.get();
                stage.click(x, y);
                result.set(editor.borrow().as_ref().expect("editor").caret());
            }
        },
    )
    .expect("render");
    assert_eq!(result.get(), expected.get());
}

#[test]
fn repeated_find_previous_walks_back_through_the_matches() {
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::{App, run_app};

    use crate::find::Query;

    struct Empty;

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("find").size(Dip(300.0), Dip(200.0)),
        |ui| {
            let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
            editor.set_text("ab ab ab");
            let query = Query::literal("ab");
            let mut seen = Vec::new();
            for _ in 0..4 {
                assert!(editor.find_next(&query, true, false).expect("valid"));
                seen.push(editor.selection().expect("selected"));
            }
            assert_eq!(seen, [(6, 8), (3, 5), (0, 2), (6, 8)]);
            let mut seen = Vec::new();
            for _ in 0..3 {
                assert!(editor.find_next(&query, true, true).expect("valid"));
                seen.push(editor.selection().expect("selected"));
            }
            assert_eq!(seen, [(0, 2), (3, 5), (6, 8)]);
            Empty
        },
    )
    .expect("run_app");
}

#[test]
fn programmatic_edits_and_find_work_on_a_live_widget() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::{App, run_app};

    use crate::find::Query;

    struct Empty;

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    let check = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&check);
    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("edits").size(Dip(300.0), Dip(200.0)),
        move |ui| {
            let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
            assert!(editor.insert_text("fn form_load() {\n}\n"));
            assert_eq!(editor.text(), "fn form_load() {\n}\n");

            let query = Query::literal("form_load");
            assert!(editor.find_next(&query, true, true).expect("valid query"));
            assert_eq!(editor.selection(), Some((3, 12)));
            assert!(editor.replace(3, 12, "form_resize"));
            assert_eq!(editor.text(), "fn form_resize() {\n}\n");

            editor.set_caret(0);
            assert_eq!(editor.caret(), 0);
            *sink.borrow_mut() = Some(());
            Empty
        },
    )
    .expect("run_app");
    assert!(check.borrow().is_some());
}

#[test]
fn the_edit_menu_commands_change_and_restore_the_buffer() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::{App, run_app};

    struct Empty;
    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    let check = Rc::new(RefCell::new(None));
    let sink = Rc::clone(&check);
    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("edit-menu").size(Dip(300.0), Dip(200.0)),
        move |ui| {
            let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200))
                .expect("editor")
                // The thread-local clipboard, so the test neither needs an OS
                // clipboard (headless CI) nor clobbers the developer's.
                .with_clipboard(crate::platform::InProcessClipboard);
            editor.set_text("hello world");
            editor.select_all();
            assert!(editor.cut(), "cut removed the selection");
            assert_eq!(editor.text(), "");
            assert!(editor.paste(), "paste restored it");
            assert_eq!(editor.text(), "hello world");
            assert!(editor.undo(), "undo removed the paste");
            assert_eq!(editor.text(), "");
            assert!(editor.redo(), "redo re-applied the paste");
            assert_eq!(editor.text(), "hello world");

            // Copy keeps the text; delete_selection clears it.
            editor.select(0, 5);
            assert!(editor.copy(), "copy found a selection");
            assert!(editor.delete_selection(), "delete cleared the selection");
            assert_eq!(editor.text(), " world");
            *sink.borrow_mut() = Some(());
            Empty
        },
    )
    .expect("run_app");
    assert!(check.borrow().is_some());
}

#[cfg(feature = "rhai-syntax")]
#[test]
fn switching_the_highlighter_on_a_live_editor_relexes_the_whole_buffer() {
    use std::cell::RefCell;
    use std::rc::Rc;

    use xui_canvas::OffscreenBackend;
    use xui_core::backend::PlatformSpec;
    use xui_core::geometry::Rect;
    use xui_core::units::Dip;
    use xui_core::{App, Image, run_app};

    struct Empty;

    impl App for Empty {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut xui_core::Ui<()>) {}
    }

    let shots: Rc<RefCell<Vec<Image>>> = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&shots);
    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("switch").size(Dip(300.0), Dip(200.0)),
        move |ui| {
            let editor = crate::Editor::new(ui, Rect::new(0, 0, 300, 200)).expect("editor");
            editor.set_text("let x = 1;\nlet y = 2;\n");
            if let Ok(image) = ui.capture() {
                sink.borrow_mut().push(image);
            }
            editor.set_highlighter(crate::RhaiHighlighter);
            if let Ok(image) = ui.capture() {
                sink.borrow_mut().push(image);
            }
            Empty
        },
    )
    .expect("run_app");

    let shots = shots.borrow();
    assert_eq!(shots.len(), 2);
    assert_ne!(
        shots[0], shots[1],
        "the plain and Rhai renders differ, so the whole buffer was re-lexed"
    );
}

use crate::platform::Clipboard;

struct SafeClipboard;

impl Clipboard for SafeClipboard {
    fn text(&self) -> Option<String> {
        None
    }
    fn set_text(&self, _text: &str) {}
}
