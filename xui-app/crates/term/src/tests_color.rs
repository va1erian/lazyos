//! Colours: schemes, the 256-colour mapping, 24-bit colour, OSC set and
//! query, and the runs a painter draws.

use crate::cell::{Attr, Color, Flags};
use crate::palette::{parse_color, scheme_by_name, xterm_256, Rgb, NAMES};
use crate::render::runs;
use crate::Term;

#[test]
fn the_cube_and_the_grey_ramp_follow_xterm() {
    let ansi = scheme_by_name("desktop", true).ansi;
    assert_eq!(xterm_256(16, &ansi), Rgb(0, 0, 0));
    assert_eq!(xterm_256(21, &ansi), Rgb(0, 0, 255));
    assert_eq!(xterm_256(196, &ansi), Rgb(255, 0, 0));
    assert_eq!(xterm_256(46, &ansi), Rgb(0, 255, 0));
    assert_eq!(xterm_256(231, &ansi), Rgb(255, 255, 255));
    assert_eq!(xterm_256(67, &ansi), Rgb(95, 135, 175), "16 + 36*1 + 6*2 + 3");
    assert_eq!(xterm_256(232, &ansi), Rgb(8, 8, 8));
    assert_eq!(xterm_256(255, &ansi), Rgb(238, 238, 238));
    for i in 0..16u8 {
        assert_eq!(xterm_256(i, &ansi), ansi[usize::from(i)]);
    }
}

#[test]
fn every_scheme_is_complete_and_legible() {
    for name in NAMES {
        for dark in [true, false] {
            let scheme = scheme_by_name(name, dark);
            // Solarized is deliberately low-contrast; the desktop's own is not.
            let floor = if name == "desktop" { 4.5 } else { 3.5 };
            assert!(scheme.fg.contrast(scheme.bg) >= floor, "{name}: text on its background");
            assert!(scheme.cursor.contrast(scheme.bg) >= 3.0, "{name}: the cursor shows");
            assert_eq!(scheme.ansi.len(), 16);
        }
    }
    assert_eq!(scheme_by_name("desktop", true).name, "desktop");
    assert_ne!(scheme_by_name("desktop", true).bg, scheme_by_name("desktop", false).bg);
    assert_eq!(scheme_by_name("nonsense", false), scheme_by_name("desktop", false));
    assert_eq!(scheme_by_name("tango", true), scheme_by_name("tango", false), "named schemes do not follow the mode");
}

#[test]
fn colour_specs_parse() {
    assert_eq!(parse_color("rgb:ff/80/00"), Some(Rgb(255, 128, 0)));
    assert_eq!(parse_color("rgb:ffff/8080/0000"), Some(Rgb(255, 128, 0)));
    assert_eq!(parse_color("rgb:f/0/8"), Some(Rgb(255, 0, 136)));
    assert_eq!(parse_color("#ff8000"), Some(Rgb(255, 128, 0)));
    assert_eq!(parse_color("#f80"), Some(Rgb(255, 136, 0)));
    assert_eq!(parse_color("#fff800000"), Some(Rgb(255, 128, 0)));
    for bad in ["", "rgb:", "rgb:1/2", "rgb:1/2/3/4", "rgb:zz/00/00", "#12", "#1234567", "red", "rgb:fffff/0/0"] {
        assert_eq!(parse_color(bad), None, "{bad:?}");
    }
}

#[test]
fn the_palette_resolves_cell_colours() {
    let term = Term::new(10, 2);
    let palette = term.palette();
    let ansi = palette.scheme().ansi;
    assert_eq!(palette.resolve(Color::Default, true, false, false), palette.fg());
    assert_eq!(palette.resolve(Color::Default, false, false, false), palette.bg());
    assert_eq!(palette.resolve(Color::Indexed(1), true, false, false), ansi[1]);
    assert_eq!(palette.resolve(Color::Indexed(1), true, true, false), ansi[1], "bold alone keeps the colour");
    assert_eq!(palette.resolve(Color::Indexed(1), true, true, true), ansi[9], "bold is bright");
    assert_eq!(palette.resolve(Color::Indexed(1), false, true, true), ansi[1], "only the foreground brightens");
    assert_eq!(palette.resolve(Color::Indexed(9), true, true, true), ansi[9], "bright stays");
    assert_eq!(palette.resolve(Color::Indexed(200), true, false, false), xterm_256(200, &ansi));
    assert_eq!(palette.resolve(Color::Rgb(1, 2, 3), false, false, false), Rgb(1, 2, 3));
}

#[test]
fn a_program_can_change_and_query_colours() {
    let mut term = Term::new(10, 2);
    term.feed(b"\x1b]4;1;rgb:ff/00/00\x07");
    assert_eq!(term.palette().get(1), Rgb(255, 0, 0));
    term.feed(b"\x1b]4;1;?\x07");
    assert_eq!(term.take_reply(), b"\x1b]4;1;rgb:ffff/0000/0000\x07");
    term.feed(b"\x1b]4;2;#00ff00;3;rgb:00/00/ff\x1b\\");
    assert_eq!((term.palette().get(2), term.palette().get(3)), (Rgb(0, 255, 0), Rgb(0, 0, 255)));
    term.feed(b"\x1b]10;#102030\x1b\\\x1b]11;#405060\x07\x1b]12;#708090\x07");
    let p = term.palette();
    assert_eq!((p.fg(), p.bg(), p.cursor()), (Rgb(0x10, 0x20, 0x30), Rgb(0x40, 0x50, 0x60), Rgb(0x70, 0x80, 0x90)));
    term.feed(b"\x1b]11;?\x1b\\");
    assert_eq!(term.take_reply(), b"\x1b]11;rgb:4040/5050/6060\x1b\\", "the reply ends as the query did");
    term.feed(b"\x1b]10;?\x07\x1b]12;?\x07");
    assert_eq!(term.take_reply(), b"\x1b]10;rgb:1010/2020/3030\x07\x1b]12;rgb:7070/8080/9090\x07");
}

#[test]
fn colours_reset() {
    let mut term = Term::new(10, 2);
    let base = term.palette().clone();
    term.feed(b"\x1b]4;1;#010203\x07\x1b]4;5;#040506\x07\x1b]10;#111111\x07\x1b]11;#222222\x07\x1b]12;#333333\x07");
    term.feed(b"\x1b]104;1\x07");
    assert_eq!(term.palette().get(1), base.get(1));
    assert_ne!(term.palette().get(5), base.get(5));
    term.feed(b"\x1b]110\x07\x1b]111\x07\x1b]112\x07");
    assert_eq!((term.palette().fg(), term.palette().bg(), term.palette().cursor()), (base.fg(), base.bg(), base.cursor()));
    term.feed(b"\x1b]104\x07");
    assert_eq!(*term.palette(), base);
    term.feed(b"\x1b]4;7;#abcdef\x07\x1bc");
    assert_eq!(*term.palette(), base, "RIS resets the colours");
}

#[test]
fn a_new_scheme_drops_what_a_program_changed() {
    let mut term = Term::new(10, 2);
    term.feed(b"\x1b]4;1;#010203\x07");
    term.set_scheme(scheme_by_name("gruvbox-dark", true));
    assert_eq!(term.palette().get(1), Rgb::hex(0xcc241d));
    assert_eq!(term.palette().bg(), Rgb::hex(0x282828));
    term.feed(b"\x1bc");
    assert_eq!(term.palette().bg(), Rgb::hex(0x282828), "RIS keeps the scheme");
}

#[test]
fn malformed_colour_commands_are_ignored() {
    let mut term = Term::new(10, 2);
    let base = term.palette().clone();
    for bytes in [
        &b"\x1b]4\x07"[..],
        b"\x1b]4;300;#ffffff\x07",
        b"\x1b]4;x;#ffffff\x07",
        b"\x1b]4;1\x07",
        b"\x1b]10;nonsense\x07",
        b"\x1b]11\x07",
        b"\x1b]12;rgb:1/2/3/4\x07",
        b"\x1b]999;\xff\xfe\x07",
    ] {
        term.feed(bytes);
    }
    assert_eq!(*term.palette(), base);
    assert!(term.take_reply().is_empty());
    // A pile of pairs is bounded, not a loop of a million.
    let many: String = (0..1000).map(|i| format!(";{};#ffffff", i % 256)).collect();
    term.feed(format!("\x1b]4{many}\x07").as_bytes());
}

#[test]
fn true_colour_and_256_colour_sgr_reach_the_cells() {
    let mut term = Term::new(10, 1);
    term.feed(b"\x1b[38;2;10;20;30;48;5;196mx\x1b[0m");
    let cell = term.row(0).cells[0];
    assert_eq!(cell.attr.fg, Color::Rgb(10, 20, 30));
    assert_eq!(cell.attr.bg, Color::Indexed(196));
    assert_eq!(term.palette().resolve(cell.attr.bg, false, false, false), Rgb(255, 0, 0));
}

#[test]
fn runs_merge_equal_attributes_and_say_where_they_start() {
    let mut term = Term::new(12, 1);
    term.feed(b"ab\x1b[1mcd\x1b[0m ef\x1b[31mg");
    let found = runs(&term.row(0).cells, 12);
    let summary: Vec<(usize, usize, &str)> = found.iter().map(|r| (r.col, r.cells, r.text.as_str())).collect();
    assert_eq!(summary, [(0, 2, "ab"), (2, 2, "cd"), (4, 3, " ef"), (7, 1, "g")]);
    assert!(found[1].attr.flags.has(Flags::BOLD));
    assert_eq!(found[3].attr.fg, Color::Indexed(1));
}

#[test]
fn a_wide_character_is_one_glyph_over_two_cells() {
    let mut term = Term::new(8, 1);
    term.feed("a\u{4e2d}b".as_bytes());
    let found = runs(&term.row(0).cells, 8);
    assert_eq!(found.len(), 1, "one attribute, one run");
    let run = &found[0];
    assert_eq!((run.col, run.cells, run.text.as_str()), (0, 4, "a\u{4e2d}b"));
    // Cut at the wide character's second cell it still covers two.
    let cut = runs(&term.row(0).cells, 2);
    assert_eq!(cut[0].cells, 2, "clipped to the edge, never past it");
}

#[test]
fn a_blank_default_tail_makes_no_runs_but_a_background_does() {
    let mut term = Term::new(10, 1);
    term.feed(b"hi");
    assert_eq!(runs(&term.row(0).cells, 10).len(), 1);
    term.feed(b"\x1b[44m   \x1b[0m");
    let found = runs(&term.row(0).cells, 10);
    assert_eq!(found.len(), 2);
    assert_eq!((found[1].col, found[1].cells, found[1].attr.bg), (2, 3, Color::Indexed(4)));
    assert!(found[1].is_blank());
    assert!(runs(&[], 10).is_empty(), "a trimmed history line");
}

#[test]
fn the_default_attribute_is_the_blank_cell() {
    assert_eq!(Attr::default().flags, Flags::default());
}
