//! The shipped `xterm-256color` terminfo entry (`assets/terminfo`) says only
//! what the engine does: its key capabilities are the bytes `keys.rs` sends and
//! its output capabilities are sequences the parser acts on.

use crate::input::Mods;
use crate::keys::{encode_named, KeyModes, Named};
use crate::Term;

const SOURCE: &str = include_str!("../../../../assets/terminfo/xterm-256color.src");

fn entry() -> terminfo::Entry {
    terminfo::read_compiled(&terminfo::compile_source(SOURCE).unwrap()).unwrap()
}

const APP: KeyModes = KeyModes { app_cursor: true, app_keypad: true };
const NONE: Mods = Mods { shift: false, alt: false, ctrl: false };

fn mods(shift: bool, alt: bool, ctrl: bool) -> Mods {
    Mods { shift, alt, ctrl }
}

#[test]
fn the_key_capabilities_are_what_the_encoder_sends() {
    let e = entry();
    let key = |name: &str| e.string(name).unwrap_or_else(|| panic!("{name} is not in the entry")).to_vec();
    // smkx turns application cursor keys on, so these are the SS3 forms.
    let app = [
        ("kcuu1", Named::Up),
        ("kcud1", Named::Down),
        ("kcuf1", Named::Right),
        ("kcub1", Named::Left),
        ("khome", Named::Home),
        ("kend", Named::End),
    ];
    for (cap, named) in app {
        assert_eq!(key(cap), encode_named(named, NONE, APP).unwrap(), "{cap}");
    }
    let plain = [
        ("kich1", Named::Insert),
        ("kdch1", Named::Delete),
        ("kpp", Named::PageUp),
        ("knp", Named::PageDown),
    ];
    for (cap, named) in plain {
        assert_eq!(key(cap), encode_named(named, NONE, APP).unwrap(), "{cap}");
    }
    assert_eq!(key("kbs"), encode_named(Named::Backspace, NONE, APP).unwrap());
    assert_eq!(key("kcbt"), encode_named(Named::Tab, mods(true, false, false), APP).unwrap());
    // F1 to F12, then shifted (13 to 24) and Ctrl (25 to 36).
    for n in 1..=12u8 {
        for (base, m) in [(0u8, NONE), (12, mods(true, false, false)), (24, mods(false, false, true))] {
            let cap = format!("kf{}", n + base);
            assert_eq!(key(&cap), encode_named(Named::F(n), m, APP).unwrap(), "{cap}");
        }
    }
    let shifted = [
        ("kLFT", Named::Left),
        ("kRIT", Named::Right),
        ("kHOM", Named::Home),
        ("kEND", Named::End),
        ("kDC", Named::Delete),
        ("kIC", Named::Insert),
        ("kNXT", Named::PageDown),
        ("kPRV", Named::PageUp),
        ("kind", Named::Down),
        ("kri", Named::Up),
    ];
    for (cap, named) in shifted {
        assert_eq!(key(cap), encode_named(named, mods(true, false, false), APP).unwrap(), "{cap}");
    }
}

#[test]
fn smkx_and_rmkx_set_the_modes_the_keys_assume() {
    let e = entry();
    let mut term = Term::new(20, 4);
    term.feed(e.string("smkx").unwrap());
    assert_eq!(term.key_modes(), APP);
    term.feed(e.string("rmkx").unwrap());
    assert_eq!(term.key_modes(), KeyModes::default());
}

/// Feed one capability to a fresh terminal holding `before` and return the
/// rows as text.
fn run(before: &[u8], cap: &[u8]) -> (Term, Vec<String>) {
    let mut term = Term::new(10, 4);
    term.feed(before);
    term.feed(cap);
    let rows = (0..4).map(|r| term.row_text(r)).collect();
    (term, rows)
}

#[test]
fn the_output_capabilities_do_what_they_say() {
    let e = entry();
    let cap = |name: &str| e.string(name).unwrap_or_else(|| panic!("{name}")).to_vec();
    let (_, rows) = run(b"abc\r\ndef", &cap("clear"));
    assert_eq!(rows, ["", "", "", ""], "clear");
    let (t, _) = run(b"abc\r\ndef", &cap("home"));
    assert_eq!(t.cursor(), (0, 0), "home");
    let (_, rows) = run(b"abcdef\x1b[1;4H", &cap("el"));
    assert_eq!(rows[0], "abc", "el");
    let (_, rows) = run(b"abcdef\x1b[1;4H", &cap("el1"));
    assert_eq!(rows[0], "    ef", "el1");
    let (_, rows) = run(b"abc\r\ndef\x1b[1;1H", &cap("ed"));
    assert_eq!(rows, ["", "", "", ""], "ed");
    let (t, _) = run(b"\x1b[3;3H", &cap("cuu1"));
    assert_eq!(t.cursor(), (1, 2), "cuu1");
    let (t, _) = run(b"\x1b[1;1H", &cap("cuf1"));
    assert_eq!(t.cursor(), (0, 1), "cuf1");
    let (t, _) = run(b"\x1b[3;3H", &cap("cub1"));
    assert_eq!(t.cursor(), (2, 1), "cub1");
    let (t, _) = run(b"ab\x1b7\x1b[3;3H", &cap("rc"));
    assert_eq!(t.cursor(), (0, 2), "sc/rc: ESC 7 then rc");
    let (t, _) = run(b"", &cap("civis"));
    assert!(!t.cursor_visible(), "civis");
    let (t, _) = run(&cap("civis"), &cap("cnorm"));
    assert!(t.cursor_visible(), "cnorm");
    let (t, _) = run(b"main", &cap("smcup"));
    assert!(t.on_alternate_screen(), "smcup");
    let (t, rows) = run(&[&b"main"[..], &cap("smcup"), b"alt"].concat(), &cap("rmcup"));
    assert!(!t.on_alternate_screen() && rows[0] == "main", "rmcup");
    let (t, _) = run(&cap("bold"), b"x");
    assert!(t.row(0).cells[0].attr.flags.has(crate::Flags::BOLD), "bold");
    let (t, _) = run(&[&cap("bold")[..], &cap("sgr0")[..]].concat(), b"x");
    assert!(!t.row(0).cells[0].attr.flags.has(crate::Flags::BOLD), "sgr0");
    let (t, _) = run(&cap("sitm"), b"x");
    assert!(t.row(0).cells[0].attr.flags.has(crate::Flags::ITALIC), "sitm");
    let (t, _) = run(&cap("rev"), b"x");
    assert!(t.row(0).cells[0].attr.flags.has(crate::Flags::INVERSE), "rev");
    let (t, _) = run(&[&cap("rev")[..], &cap("rmso")[..]].concat(), b"x");
    assert!(!t.row(0).cells[0].attr.flags.has(crate::Flags::INVERSE), "rmso");
    let (t, _) = run(b"\x1b[31;42m", &cap("op"));
    let attr = {
        let mut t = t;
        t.feed(b"x");
        t.row(0).cells[0].attr
    };
    assert_eq!((attr.fg, attr.bg), (crate::Color::Default, crate::Color::Default), "op");
    let (mut t, _) = run(b"abc", &cap("u7"));
    assert_eq!(t.take_reply(), b"\x1b[1;4R", "u7 asks for a report that u6 describes");
    let (mut t, _) = run(&cap("u9"), b"");
    assert_eq!(t.take_reply(), b"\x1b[?62;22c", "u9");
}

#[test]
fn the_colour_capabilities_expand_to_sequences_the_parser_takes() {
    // setaf/setab are `%?...%;` conditionals: the three forms they produce.
    let e = entry();
    let setaf = e.string("setaf").unwrap();
    assert!(setaf.windows(2).any(|w| w == b"%?"), "a conditional");
    let mut t = Term::new(10, 2);
    t.feed(b"\x1b[31mA\x1b[91mB\x1b[38:5:200mC\x1b[0m");
    let c = |col: usize| t.row(0).cells[col].attr.fg;
    assert_eq!(c(0), crate::Color::Indexed(1));
    assert_eq!(c(1), crate::Color::Indexed(9));
    assert_eq!(c(2), crate::Color::Indexed(200));
    assert_eq!(e.number("colors"), Some(256));
}

#[test]
fn nothing_unimplemented_is_claimed() {
    let e = entry();
    for cap in ["acsc", "smacs", "rmacs", "smir", "rmir", "blink", "enacs", "rs2", "is2", "flash"] {
        assert!(e.string(cap).is_none(), "{cap} would promise something lazyterm lacks");
    }
    assert!(!e.flag("mir") && !e.flag("km"));
}
