//! Mouse encodings, the modes that turn them on, and paste sanitising.

use crate::input::{mouse_report, Action, Button, Mods, MouseMode};
use crate::paste::{line_count, needs_confirmation, paste_bytes, shell_quote};
use crate::Term;

const NONE: Mods = Mods {
    shift: false,
    alt: false,
    ctrl: false,
};

fn report(
    mode: MouseMode,
    sgr: bool,
    button: Option<Button>,
    action: Action,
    col: usize,
    row: usize,
) -> Option<Vec<u8>> {
    mouse_report(mode, sgr, button, action, NONE, col, row)
}

#[test]
fn x10_press_release_and_wheel() {
    let click = MouseMode::Click;
    let press = report(click, false, Some(Button::Left), Action::Press, 0, 0);
    assert_eq!(press.unwrap(), b"\x1b[M !!");
    let release = report(click, false, Some(Button::Left), Action::Release, 4, 9);
    assert_eq!(
        release.unwrap(),
        vec![0x1b, b'[', b'M', 35, 33 + 4, 33 + 9],
        "a release is button 3"
    );
    let wheel = report(click, false, Some(Button::WheelUp), Action::Press, 1, 1);
    assert_eq!(wheel.unwrap(), vec![0x1b, b'[', b'M', 32 + 64, 34, 34]);
    assert!(report(click, false, Some(Button::WheelUp), Action::Release, 1, 1).is_none());
}

#[test]
fn sgr_press_release_and_wheel() {
    let click = MouseMode::Click;
    let right = Some(Button::Right);
    assert_eq!(report(click, true, right, Action::Press, 9, 19).unwrap(), b"\x1b[<2;10;20M");
    assert_eq!(report(click, true, right, Action::Release, 9, 19).unwrap(), b"\x1b[<2;10;20m");
    let down = Some(Button::WheelDown);
    assert_eq!(report(click, true, down, Action::Press, 4, 5).unwrap(), b"\x1b[<65;5;6M");
    let middle = Some(Button::Middle);
    assert_eq!(report(click, true, middle, Action::Press, 0, 0).unwrap(), b"\x1b[<1;1;1M");
}

#[test]
fn modifiers_and_motion_add_to_the_code() {
    let mods = Mods {
        shift: true,
        alt: true,
        ctrl: true,
    };
    let left = Some(Button::Left);
    let all = mouse_report(MouseMode::Drag, true, left, Action::Motion, mods, 0, 0);
    assert_eq!(all.unwrap(), b"\x1b[<60;1;1M", "0 + 4 + 8 + 16 + 32");
}

#[test]
fn the_levels_report_what_they_promise() {
    let left = Some(Button::Left);
    assert!(report(MouseMode::Off, true, left, Action::Press, 0, 0).is_none());
    assert!(report(MouseMode::Click, true, left, Action::Motion, 0, 0).is_none());
    assert!(report(MouseMode::Drag, true, None, Action::Motion, 0, 0).is_none());
    assert!(report(MouseMode::Drag, true, left, Action::Motion, 0, 0).is_some());
    let any = report(MouseMode::Motion, true, None, Action::Motion, 2, 3);
    assert_eq!(any.unwrap(), b"\x1b[<35;3;4M");
}

#[test]
fn x10_coordinates_are_clamped_to_a_byte() {
    let left = Some(Button::Left);
    let far = report(MouseMode::Click, false, left, Action::Press, 5000, 5000).unwrap();
    assert_eq!(far, vec![0x1b, b'[', b'M', 32, 255, 255]);
    let sgr = report(MouseMode::Click, true, left, Action::Press, 5000, 1).unwrap();
    assert_eq!(sgr, b"\x1b[<0;5001;2M", "SGR has no limit");
}

#[test]
fn the_program_turns_modes_on_and_off() {
    let mut term = Term::new(10, 3);
    assert_eq!(term.mouse_mode(), MouseMode::Off);
    term.feed(b"\x1b[?1000h");
    assert_eq!(term.mouse_mode(), MouseMode::Click);
    term.feed(b"\x1b[?1002h");
    assert_eq!(term.mouse_mode(), MouseMode::Drag);
    term.feed(b"\x1b[?1000l");
    assert_eq!(term.mouse_mode(), MouseMode::Drag, "a different level stays");
    term.feed(b"\x1b[?1002l");
    assert_eq!(term.mouse_mode(), MouseMode::Off);
    term.feed(b"\x1b[?1003h\x1b[?1006h\x1b[?2004h");
    let state = (term.mouse_mode(), term.mouse_sgr(), term.bracketed_paste());
    assert_eq!(state, (MouseMode::Motion, true, true));
    term.feed(b"\x1bc");
    let state = (term.mouse_mode(), term.mouse_sgr(), term.bracketed_paste());
    assert_eq!(state, (MouseMode::Off, false, false));
}

#[test]
fn pasted_newlines_become_returns() {
    assert_eq!(paste_bytes("a\r\nb\nc\rd", false), b"a\rb\rc\rd");
    assert_eq!(paste_bytes("tab\there", false), b"tab\there");
}

#[test]
fn pasted_controls_are_dropped() {
    assert_eq!(paste_bytes("a\x00b\x07c\x08d\x7fe\u{85}f\x1bg", false), b"abcdefg");
}

#[test]
fn bracketed_paste_wraps_and_cannot_be_escaped() {
    let out = paste_bytes("rm -rf /\x1b[201~; evil", true);
    assert!(out.starts_with(b"\x1b[200~") && out.ends_with(b"\x1b[201~"));
    let inner = &out[6..out.len() - 6];
    assert!(!inner.contains(&0x1b), "no ESC inside the bracket");
    assert_eq!(inner, b"rm -rf /[201~; evil");
}

#[test]
fn multi_line_pastes_need_confirmation_unless_bracketed() {
    assert!(!needs_confirmation("one line", false));
    assert!(needs_confirmation("a\nb", false));
    assert!(needs_confirmation("a\n", false), "a trailing newline runs the command");
    assert!(!needs_confirmation("a\nb", true));
    assert_eq!(line_count("a\nb\nc"), 3);
    assert_eq!(line_count("a\r\nb\r\n"), 2);
    assert_eq!(line_count(""), 0);
    // A selection that starts on blank cells begins with an empty line: it is
    // a line (the shell would run it), so it counts; a trailing newline ends
    // the last line and starts none.
    assert_eq!(line_count("\n39\n40\n41\n42\n4"), 6);
    assert_eq!(line_count("38\n39\n40\n41\n42\n"), 5);
    assert_eq!(line_count("one"), 1);
    assert_eq!(line_count("\n"), 1);
}

#[test]
fn paths_are_quoted_for_the_shell() {
    assert_eq!(shell_quote("dir/a.txt"), "dir/a.txt");
    assert_eq!(shell_quote("dir/my file"), "'dir/my file'");
    assert_eq!(shell_quote("it's"), "'it'\\''s'");
    assert_eq!(shell_quote("a;rm -rf *"), "'a;rm -rf *'");
    assert_eq!(shell_quote("$(x)"), "'$(x)'");
    assert_eq!(shell_quote(""), "''");
    assert_eq!(shell_quote("a\nb"), "'ab'", "controls never reach the shell");
}
