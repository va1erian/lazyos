//! Tests for the preferences model (`prefs.rs`).

use super::*;

const NONE: Modifiers = Modifiers { ctrl: false, shift: false, alt: false, win: false };
const SHIFT: Modifiers = Modifiers { shift: true, ..NONE };

fn prefs() -> Prefs {
    Prefs::new(&Settings::default(), vec!["JetBrains Mono".into(), "Liberation Mono".into(), "Other Mono".into()])
}

fn only(out: &Outcome) -> (&'static str, Value) {
    assert_eq!(out.changes.len(), 1, "{out:?}");
    (out.changes[0].key, out.changes[0].value.clone())
}

#[test]
fn the_stepper_controls_change_by_their_steps_and_stay_in_bounds() {
    let mut p = prefs();
    p.focus = Ctl::Size;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("font/size", Value::Int(16)));
    assert_eq!(only(&p.key(Key::LEFT, NONE)), ("font/size", Value::Int(15)));
    for _ in 0..100 {
        p.key(Key::LEFT, NONE);
    }
    assert_eq!(p.settings.size, 6);
    for _ in 0..100 {
        p.key(Key::RIGHT, NONE);
    }
    assert_eq!(p.settings.size, 72);
    p.focus = Ctl::Spacing;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("font/line_spacing", Value::Int(135)));
    for _ in 0..40 {
        p.key(Key::RIGHT, NONE);
    }
    assert_eq!(p.settings.line_spacing, 200);
}

#[test]
fn the_choice_controls_cycle() {
    let mut p = prefs();
    p.focus = Ctl::Scheme;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("colors/scheme", Value::Text("solarized-dark".into())));
    p.key(Key::LEFT, NONE);
    assert_eq!(only(&p.key(Key::LEFT, NONE)).1, Value::Text("tango".into()), "wraps round");
    p.focus = Ctl::CursorShape;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("cursor/shape", Value::Text("underline".into())));
    p.focus = Ctl::Bell;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("bell", Value::Text("sound".into())));
    p.key(Key::RIGHT, NONE);
    p.key(Key::RIGHT, NONE);
    assert_eq!(p.settings.bell, Bell::None, "wrapped past notify");
    p.focus = Ctl::Family;
    assert_eq!(only(&p.key(Key::DOWN, NONE)), ("font/family", Value::Text("Liberation Mono".into())));
    p.key(Key::DOWN, NONE);
    assert_eq!(p.key(Key::DOWN, NONE).changes[0].value, Value::Text("Other Mono".into()), "stops at the last");
}

#[test]
fn toggles_write_booleans() {
    let mut p = prefs();
    p.focus = Ctl::CopyOnSelect;
    assert_eq!(only(&p.char(' ')), ("select/copy_on_select", Value::Bool(true)));
    assert_eq!(only(&p.key(Key::RETURN, NONE)), ("select/copy_on_select", Value::Bool(false)));
    p.focus = Ctl::ConfirmPaste;
    assert_eq!(only(&p.key(Key::SPACE, NONE)), ("paste/confirm_multiline", Value::Bool(false)));
    p.focus = Ctl::Blink;
    assert_eq!(only(&p.key(Key::RIGHT, NONE)), ("cursor/blink", Value::Bool(false)));
    p.focus = Ctl::Bold;
    assert_eq!(only(&p.key(Key::LEFT, NONE)), ("colors/bold_is_bright", Value::Bool(true)));
}

#[test]
fn text_fields_validate_before_they_write() {
    let mut p = prefs();
    p.focus = Ctl::Scrollback;
    p.key(Key::END, NONE);
    for _ in 0..6 {
        p.key(Key::BACK, NONE);
    }
    assert_eq!(p.char('x'), Outcome::default(), "letters are not digits");
    for c in "2500".chars() {
        p.char(c);
    }
    assert_eq!(only(&p.key(Key::RETURN, NONE)), ("scrollback/lines", Value::Int(2500)));
    for _ in 0..4 {
        p.key(Key::BACK, NONE);
    }
    for c in "999999999".chars() {
        p.char(c);
    }
    let out = p.key(Key::RETURN, NONE);
    assert!(out.changes.is_empty() && p.error.is_some(), "over the limit is refused");
    // Window size.
    p.focus = Ctl::WindowSize;
    for c in "100x30".chars() {
        p.char(c);
    }
    assert_eq!(only(&p.key(Key::RETURN, NONE)), ("window/size", Value::Text("100x30".into())));
    for _ in 0..3 {
        p.key(Key::BACK, NONE);
    }
    p.char('9');
    p.char('9');
    p.char('9');
    p.char('9');
    let out = p.key(Key::RETURN, NONE);
    assert!(out.changes.is_empty() && p.error.is_some(), "100x9999 is out of range");
    // Word characters accept anything printable, bounded.
    p.focus = Ctl::WordChars;
    p.key(Key::END, NONE);
    p.char('#');
    assert_eq!(only(&p.key(Key::RETURN, NONE)).0, "select/word_chars");
    for _ in 0..200 {
        p.char('y');
    }
    assert!(p.word_chars.text.chars().count() <= FIELD_LIMIT);
}

#[test]
fn tab_moves_focus_and_commits_the_field_it_leaves() {
    let mut p = prefs();
    p.focus = Ctl::Scrollback;
    p.key(Key::END, NONE);
    p.key(Key::BACK, NONE);
    let out = p.key(Key::TAB, NONE);
    assert_eq!(only(&out).0, "scrollback/lines");
    assert_eq!(p.focus, Ctl::Bell);
    p.key(Key::TAB, SHIFT);
    assert_eq!(p.focus, Ctl::Scrollback);
    // Round trip through the order.
    p.focus = Ctl::Family;
    for _ in 0..ORDER.len() {
        p.key(Key::TAB, NONE);
    }
    assert_eq!(p.focus, Ctl::Family);
    p.key(Key::TAB, SHIFT);
    assert_eq!(p.focus, Ctl::WindowSize);
}

#[test]
fn escape_closes_and_discards_a_half_typed_field() {
    let mut p = prefs();
    p.focus = Ctl::WindowSize;
    p.char('1');
    let out = p.key(Key::ESCAPE, NONE);
    assert!(out.close && out.changes.is_empty());
}

#[test]
fn a_change_made_elsewhere_shows_without_disturbing_an_edit() {
    let mut p = prefs();
    p.focus = Ctl::WordChars;
    p.key(Key::END, NONE);
    p.char('Z');
    let s = Settings { size: 22, scrollback: 777, ..Settings::default() };
    p.sync(&s);
    assert_eq!(p.settings.size, 22);
    assert_eq!(p.scrollback.text, "777");
    assert!(p.word_chars.text.ends_with('Z'), "the field being edited is kept");
}

#[test]
fn clicks_write_and_take_the_focus() {
    let mut p = prefs();
    let laid = layout(&p, (640, 400), 1);
    let (_, rect, _) = laid.parts.iter().find(|(part, _, _)| *part == Part::Scheme(3)).unwrap();
    let part = laid.part_at(rect.left + 2, rect.top + 2).unwrap().clone();
    let out = p.click_part(part);
    assert_eq!(only(&out), ("colors/scheme", Value::Text("gruvbox-dark".into())));
    assert_eq!(p.focus, Ctl::Scheme);
    let out = p.click_part(Part::Family("Liberation Mono".into()));
    assert_eq!(only(&out).1, Value::Text("Liberation Mono".into()));
    assert!(p.click_part(Part::Close).close);
    // Clicking away from a text field commits it.
    p.focus = Ctl::Scrollback;
    p.char('1');
    let out = p.click_part(Part::Cursor(2));
    assert_eq!(out.changes.len(), 2, "{out:?}");
    assert_eq!(out.changes[0].key, "scrollback/lines");
}

#[test]
fn the_layout_has_a_named_rectangle_for_every_control_inside_the_panel() {
    let p = prefs();
    for scale in [1, 2] {
        let laid = layout(&p, (640 * scale, 440 * scale), scale);
        let names: Vec<&str> = laid.parts.iter().map(|(_, _, n)| n.as_str()).collect();
        for want in [
            "prefs:size_dec",
            "prefs:size_inc",
            "prefs:scheme:tango",
            "prefs:cursor:bar",
            "prefs:blink",
            "prefs:scrollback",
            "prefs:bell:notify",
            "prefs:copy_on_select",
            "prefs:confirm_paste",
            "prefs:word_chars",
            "prefs:window_size",
            "prefs:family:JetBrains_Mono",
            "prefs:close",
        ] {
            assert!(names.contains(&want), "{want}");
        }
        for (_, r, name) in &laid.parts {
            assert!(r.left >= laid.panel.left && r.right <= laid.panel.right, "{name}: {r:?} in {:?}", laid.panel);
            assert!(r.top >= laid.panel.top && r.bottom <= laid.panel.bottom, "{name}");
        }
    }
}
