//! Snapshots of selection, scrolling and the overlays.

use super::*;

#[test]
fn a_selection_is_a_readable_band_behind_the_text() {
    for (dark, tag) in [(true, "dark"), (false, "light")] {
        let mut term = Term::new(80, 19);
        term.feed(b"select these words please\r\nsecond row here");
        term.select_begin(SelectMode::Char, Point::new(0, 7));
        term.select_update(Point::new(1, 5));
        let shot = Shot { dark, ..Shot::default() };
        let image = shoot(term, &shot, &format!("term-selection-{tag}.png"));
        let scheme = scheme_by_name("desktop", dark);
        let accent = if dark { Theme::dark().accent } else { Theme::light().accent };
        let band = selection_color(&lazyterm::Palette::new(scheme), Rgb(accent.r, accent.g, accent.b));
        assert_eq!(image.corner(7, 0), band, "{tag}: first selected cell");
        assert_eq!(image.corner(30, 0), band, "{tag}: the rest of the first row");
        assert_eq!(image.corner(5, 1), band, "{tag}: last selected cell");
        assert_eq!(image.corner(6, 1), scheme.bg, "{tag}: after it");
        assert_eq!(image.corner(6, 0), scheme.bg, "{tag}: before it");
        assert!(scheme.fg.contrast(band) >= 4.5, "{tag}: text on the selection");
        assert_ne!(band, scheme.bg, "{tag}: the selection shows");
    }
}

#[test]
fn the_selection_comes_from_the_theme_accent_and_stays_legible_in_every_scheme() {
    for name in NAMES {
        let scheme = scheme_by_name(name, true);
        let palette = lazyterm::Palette::new(scheme);
        for accent in [Rgb(0x4c, 0xc2, 0xff), Rgb(0x00, 0x5f, 0xb8), Rgb(255, 0, 0), Rgb(255, 255, 255)] {
            let band = selection_color(&palette, accent);
            assert!(band.contrast(scheme.fg) >= 4.5 || band == scheme.bg.lerp(accent, 0.05), "{name}: {band:?}");
        }
    }
}

#[test]
fn a_block_selection_paints_only_its_columns() {
    let mut term = Term::new(80, 19);
    term.feed(b"abcdefghij\r\nklmnopqrst\r\nuvwxyz");
    term.select_begin(SelectMode::Block, Point::new(0, 2));
    term.select_update(Point::new(2, 4));
    let image = shoot(term, &Shot::default(), "term-selection-block.png");
    let scheme = scheme_by_name("desktop", true);
    for row in 0..3 {
        assert_ne!(image.corner(3, row), scheme.bg, "row {row} column 3");
        assert_eq!(image.corner(8, row), scheme.bg, "row {row} column 8");
    }
}

#[test]
fn a_full_screen_program_paints_from_the_alternate_screen() {
    let mut term = shell();
    term.feed(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[1;1Hpager\x1b[2;1Hline two\x1b[?25l");
    let image = shoot(term, &Shot::default(), "term-alternate.png");
    let bg = scheme_by_name("desktop", true).bg;
    assert!(image.has_ink(0, 0, 5, bg) && image.has_ink(0, 1, 8, bg));
    assert!(!image.has_ink(0, 4, 20, bg), "the shell's prompt is gone from view");
}

#[test]
fn a_hidden_cursor_is_not_painted() {
    let mut term = shell();
    term.feed(b"\x1b[?25l");
    let image = shoot(term, &Shot::default(), "term-cursor-hidden.png");
    let scheme = scheme_by_name("desktop", true);
    assert_eq!(image.corner(4, 4), scheme.bg);
}

#[test]
fn a_scrolled_back_view_shows_old_lines_a_scrollbar_and_no_cursor() {
    let long = || {
        let mut term = Term::new(80, 19);
        for line in 1..=300 {
            term.feed(format!("line {line} of the output\r\n").as_bytes());
        }
        term
    };
    let at_bottom = shoot(long(), &Shot::default(), "term-scroll-bottom.png");
    let back = shoot(long(), &Shot { offset: 120, ..Shot::default() }, "term-scroll-back.png");
    let scheme = scheme_by_name("desktop", true);
    let gutter = (630..640).flat_map(|x| (10..390).map(move |y| (x, y)));
    assert!(gutter.clone().all(|(x, y)| at_bottom.pixel(x, y) == scheme.bg), "hidden at offset 0");
    assert!(gutter.into_iter().any(|(x, y)| back.pixel(x, y) != scheme.bg), "shown while scrolled back");
    let cursor = |s: &Shown| (0..400).any(|y| (10..19).any(|x| s.pixel(x, y) == scheme.cursor));
    assert!(cursor(&at_bottom), "the cursor at the bottom");
    assert!(!cursor(&back), "and not scrolled back");
}

#[test]
fn the_paste_confirmation_bar_has_two_buttons() {
    for (dark, tag) in [(true, "dark"), (false, "light")] {
        let mut term = Term::new(80, 19);
        term.feed(b"prompt");
        let shot = Shot { dark, pending: Some(3), ..Shot::default() };
        let image = shoot(term, &shot, &format!("term-confirm-{tag}.png"));
        let scheme = scheme_by_name("desktop", dark);
        let m = image.m;
        let layout = Layout { width: 640, height: 400, pad: m.pad, line_h: m.line_h, cell_w: m.cell_w, visible: 19 };
        let bar = confirm_rects(&layout);
        let button = scheme.bg.lerp(scheme.fg, 0.32);
        assert_eq!(image.pixel(bar.paste.left + 1, bar.paste.top + 1), button, "{tag}: Paste");
        assert_eq!(image.pixel(bar.cancel.left + 1, bar.cancel.top + 1), button, "{tag}: Cancel");
        assert_eq!(image.pixel(3, bar.bar.top), scheme.cursor, "{tag}: the bar's top edge");
        assert_eq!(image.pixel(3, bar.bar.top - 2), scheme.bg, "{tag}: above the bar");
    }
}

