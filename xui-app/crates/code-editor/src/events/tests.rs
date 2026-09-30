//! Tests for the event mapper: focus/capture deferral, editing keys, the
//! clipboard commands and wheel scrolling.

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::OffscreenBackend;
use xui_core::backend::{Event, NodeKind, NodeSpec, PlatformSpec};
use xui_core::geometry::Rect;
use xui_core::message::{Modifiers, MouseButton};
use xui_core::units::Dip;
use xui_core::{App, Ui, run_app};

use super::scroll::{WHEEL_COLS, WHEEL_ROWS, max_first_col, wheel_scroll};
use super::{handle, viewport};
use crate::options::Options;
use crate::platform::InProcessClipboard;
use crate::state::{EditorState, Effect};

struct Empty;

impl App for Empty {
    type Msg = ();
    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

/// Runs `check` with a live offscreen `Ui` and an editor-sized node.
fn with_ui(check: impl FnOnce(&Ui<()>, xui_core::backend::WidgetId) + 'static) {
    let check = Rc::new(RefCell::new(Some(check)));
    run_app(
        Rc::new(OffscreenBackend::new()),
        PlatformSpec::new("events").size(Dip(300.0), Dip(200.0)),
        move |ui| {
            let id = ui
                .create_node(&NodeSpec::new(NodeKind::Custom, Rect::new(0, 0, 300, 200)))
                .expect("node");
            if let Some(check) = check.borrow_mut().take() {
                check(ui, id);
            }
            Empty
        },
    )
    .expect("run_app");
}

fn state(text: &str) -> EditorState {
    EditorState::new(text, Options::default(), Box::new(InProcessClipboard))
}

/// A state that highlights Rhai, for the token-sync tests.
#[cfg(feature = "rhai-syntax")]
fn rhai_state(text: &str) -> EditorState {
    EditorState::with_highlighter(
        text,
        Options::default(),
        Box::new(InProcessClipboard),
        Box::new(crate::lexer::RhaiHighlighter),
    )
}

#[test]
fn focus_and_capture_are_deferred_not_called_under_the_borrow() {
    // The canvas backend delivers SetFocus / CaptureChanged synchronously
    // back into the mapper, so the handler must only record them.
    with_ui(|ui, id| {
        let mut state = state("hello\nworld");
        let press = Event::MouseDown {
            x: 80,
            y: 5,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        };
        handle(&mut state, ui, id, &press).expect("press is handled");
        assert_eq!(state.effects, [Effect::Focus, Effect::Capture]);
        assert!(state.focused);

        state.effects.clear();
        let release = Event::MouseUp {
            x: 80,
            y: 5,
            button: MouseButton::Left,
            modifiers: Modifiers::default(),
        };
        handle(&mut state, ui, id, &release).expect("release is handled");
        assert_eq!(state.effects, [Effect::ReleaseCapture]);
    });
}

/// A clipboard shared with the test, so it can see what was copied and
/// choose what is pasted.
#[derive(Clone, Default)]
struct Recording(Rc<RefCell<String>>);

impl crate::platform::Clipboard for Recording {
    fn text(&self) -> Option<String> {
        let text = self.0.borrow();
        (!text.is_empty()).then(|| text.clone())
    }

    fn set_text(&self, text: &str) {
        *self.0.borrow_mut() = text.to_owned();
    }
}

fn ctrl(key: xui_core::message::Key) -> Event {
    Event::KeyDown {
        key,
        modifiers: xui_core::message::Modifiers {
            ctrl: true,
            ..xui_core::message::Modifiers::NONE
        },
        repeat: 1,
        system: false,
    }
}

fn key_down(key: xui_core::message::Key) -> Event {
    Event::KeyDown {
        key,
        modifiers: xui_core::message::Modifiers::NONE,
        repeat: 1,
        system: false,
    }
}

#[test]
fn keys_that_change_nothing_do_not_report_a_change() {
    use xui_core::message::Key;

    with_ui(|ui, id| {
        let mut state = state("hello");
        handle(&mut state, ui, id, &Event::SetFocus);
        // Home, then Backspace at the top: the text is untouched.
        handle(&mut state, ui, id, &key_down(Key::HOME));
        let outcome = handle(&mut state, ui, id, &key_down(Key::BACK)).expect("handled");
        assert!(!outcome.changed, "Backspace at the start deletes nothing");
        handle(&mut state, ui, id, &key_down(Key::END));
        let outcome = handle(&mut state, ui, id, &key_down(Key::DELETE)).expect("handled");
        assert!(!outcome.changed, "Delete at the end deletes nothing");
        // Undo with no history is not a change either.
        let outcome = handle(&mut state, ui, id, &ctrl(Key::Z)).expect("handled");
        assert!(!outcome.changed);
        // A copy with the selection empty leaves the text alone.
        let outcome = handle(&mut state, ui, id, &ctrl(Key::C)).expect("handled");
        assert!(!outcome.changed);
    });
}

#[test]
fn edits_undo_and_redo_report_a_change() {
    use xui_core::message::Key;

    with_ui(|ui, id| {
        let mut state = state("hello");
        handle(&mut state, ui, id, &Event::SetFocus);
        handle(&mut state, ui, id, &key_down(Key::END));
        let outcome = handle(&mut state, ui, id, &key_down(Key::BACK)).expect("handled");
        assert!(outcome.changed);
        assert_eq!(state.buffer.text(), "hell");
        let outcome = handle(&mut state, ui, id, &ctrl(Key::Z)).expect("handled");
        assert!(outcome.changed);
        assert_eq!(state.buffer.text(), "hello");
        let outcome = handle(&mut state, ui, id, &ctrl(Key::Y)).expect("handled");
        assert!(outcome.changed);
        assert_eq!(state.buffer.text(), "hell");
    });
}

#[test]
fn copy_and_paste_go_through_an_injected_clipboard() {
    use xui_core::message::Key;

    with_ui(|ui, id| {
        let clipboard = Recording::default();
        let mut state = EditorState::new("hello", Options::default(), Box::new(clipboard.clone()));
        handle(&mut state, ui, id, &Event::SetFocus);
        handle(&mut state, ui, id, &ctrl(Key::A));
        handle(&mut state, ui, id, &ctrl(Key::C));
        assert_eq!(
            *clipboard.0.borrow(),
            "hello",
            "copy reached the injected clipboard"
        );

        *clipboard.0.borrow_mut() = "bye".to_owned();
        handle(&mut state, ui, id, &ctrl(Key::A));
        handle(&mut state, ui, id, &ctrl(Key::V));
        assert_eq!(
            state.buffer.text(),
            "bye",
            "paste read the injected clipboard"
        );
    });
}

#[test]
fn tab_and_shift_tab_report_a_change() {
    use xui_core::message::{Key, Modifiers};

    with_ui(|ui, id| {
        let mut state = state("a");
        handle(&mut state, ui, id, &Event::SetFocus);
        let key = |shift| Event::KeyDown {
            key: Key::TAB,
            modifiers: Modifiers {
                shift,
                ..Modifiers::NONE
            },
            repeat: 1,
            system: false,
        };
        let indented = handle(&mut state, ui, id, &key(false)).expect("tab");
        assert!(indented.changed);
        let outdented = handle(&mut state, ui, id, &key(true)).expect("shift+tab");
        assert!(outdented.changed);
        let nothing = handle(&mut state, ui, id, &key(true)).expect("shift+tab");
        assert!(!nothing.changed, "no indent left to remove");
    });
}

#[test]
fn typing_while_focused_changes_the_text() {
    with_ui(|ui, id| {
        let mut state = state("");
        handle(&mut state, ui, id, &Event::SetFocus);
        let outcome = handle(&mut state, ui, id, &Event::Char('x')).expect("char is handled");
        assert!(outcome.changed);
        assert_eq!(state.buffer.text(), "x");
    });
}

#[cfg(feature = "rhai-syntax")]
#[test]
fn typing_keeps_the_highlight_in_sync() {
    use crate::lexer::TokenClass;

    with_ui(|ui, id| {
        let mut state = rhai_state("let x = 1;");
        handle(&mut state, ui, id, &Event::SetFocus);
        handle(&mut state, ui, id, &Event::Char('/')).expect("first slash");
        handle(&mut state, ui, id, &Event::Char('/')).expect("second slash");
        assert_eq!(state.buffer.text(), "//let x = 1;");
        let classes: Vec<TokenClass> = state
            .highlight
            .tokens(0)
            .iter()
            .map(|token| token.class)
            .collect();
        assert_eq!(classes, [TokenClass::Comment]);
    });
}

/// A wheel event of `delta` units.
fn wheel(delta: i16, horizontal: bool, shift: bool) -> Event {
    Event::MouseWheel {
        delta,
        horizontal,
        x: 80,
        y: 80,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
    }
}

/// A long, wide document that scrolls both ways.
fn tall_state() -> EditorState {
    let line = "x".repeat(200);
    let text = vec![line.as_str(); 200].join("\n");
    state(&text)
}

#[test]
fn one_notch_toward_the_user_scrolls_down_by_the_wheel_rows() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        handle(&mut state, ui, id, &wheel(-120, false, false)).expect("wheel is handled");
        assert_eq!(state.view.first_line, WHEEL_ROWS as usize);
    });
}

#[test]
fn one_notch_away_from_the_user_scrolls_up() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        state.view.first_line = 10;
        handle(&mut state, ui, id, &wheel(120, false, false));
        assert_eq!(state.view.first_line, 10 - WHEEL_ROWS as usize);
    });
}

#[test]
fn two_notches_scroll_twice_as_far() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        handle(&mut state, ui, id, &wheel(-240, false, false));
        assert_eq!(state.view.first_line, 2 * WHEEL_ROWS as usize);
    });
}

#[test]
fn partial_deltas_add_up_to_a_notch() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        for _ in 0..3 {
            handle(&mut state, ui, id, &wheel(-40, false, false));
        }
        assert_eq!(state.view.first_line, WHEEL_ROWS as usize);
        assert_eq!(state.wheel_rows_rest, 0);
    });
}

#[test]
fn tiny_touchpad_deltas_eventually_scroll() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        handle(&mut state, ui, id, &wheel(-10, false, false));
        assert_eq!(state.view.first_line, 0, "a sliver of a line waits");
        for _ in 0..3 {
            handle(&mut state, ui, id, &wheel(-10, false, false));
        }
        assert_eq!(state.view.first_line, 1, "four slivers make a line");
    });
}

#[test]
fn wheel_scrolling_clamps_at_the_top() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        state.view.first_line = 1;
        handle(&mut state, ui, id, &wheel(120, false, false));
        assert_eq!(state.view.first_line, 0);
        assert_eq!(state.wheel_rows_rest, 0);
    });
}

#[test]
fn wheel_scrolling_clamps_at_the_bottom() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        let layout = viewport(ui, id, &state);
        let max = state.buffer.line_count() - layout.visible_lines;
        state.view.first_line = max - 1;
        handle(&mut state, ui, id, &wheel(-120, false, false));
        assert_eq!(state.view.first_line, max);
        handle(&mut state, ui, id, &wheel(-120 * 100, false, false));
        assert_eq!(state.view.first_line, max);
    });
}

#[test]
fn shift_wheel_scrolls_horizontally() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        handle(&mut state, ui, id, &wheel(-120, false, true));
        assert_eq!(state.view.first_col, WHEEL_COLS as usize);
        assert_eq!(state.view.first_line, 0);
        handle(&mut state, ui, id, &wheel(120, false, true));
        assert_eq!(state.view.first_col, 0);
    });
}

#[test]
fn a_horizontal_wheel_scrolls_columns_and_clamps() {
    with_ui(|ui, id| {
        let mut state = tall_state();
        let max = max_first_col(&state, &viewport(ui, id, &state)) as usize;
        handle(&mut state, ui, id, &wheel(-120, true, false));
        assert_eq!(state.view.first_col, WHEEL_COLS as usize);
        handle(&mut state, ui, id, &wheel(-120 * 200, true, false));
        assert_eq!(state.view.first_col, max);
        handle(&mut state, ui, id, &wheel(120 * 200, true, false));
        assert_eq!(state.view.first_col, 0);
    });
}

#[test]
fn wheel_scroll_carries_the_remainder_and_drops_it_at_an_end() {
    let mut rest = 0;
    assert_eq!(wheel_scroll(5, &mut rest, -20, 3, 50), 5);
    assert_eq!(rest, -60);
    assert_eq!(wheel_scroll(5, &mut rest, -20, 3, 50), 6);
    assert_eq!(rest, 0);
    assert_eq!(wheel_scroll(0, &mut rest, 20, 3, 50), 0);
    assert_eq!(rest, 0, "partial travel past the top is dropped");
    assert_eq!(
        wheel_scroll(0, &mut rest, -40, 3, 50),
        1,
        "reversing at the top scrolls at once"
    );
    assert_eq!(wheel_scroll(50, &mut rest, -20, 3, 50), 50);
    assert_eq!(rest, 0, "partial travel past the bottom is dropped");
    assert_eq!(
        wheel_scroll(50, &mut rest, 40, 3, 50),
        49,
        "reversing at the bottom scrolls at once"
    );
    assert_eq!(wheel_scroll(49, &mut rest, -140, 3, 50), 50);
    assert_eq!(rest, 0, "landing exactly on the bottom drops the rest");
}
