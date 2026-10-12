//! Snapshots of the layers over the grid: the context menu, the find bar with
//! highlighted matches, the link underline, the preferences panel, the bell's
//! flash and the prompts (`term-menu-*.png`, `term-find-*.png`, ...).

use lazyterm::settings::Settings;
use lazyterm::{Point, Search};

use crate::findbar::Find;
use crate::menu::{self, Item, Menu};
use crate::prefs::Prefs;
use super::*;

fn text_term() -> Term {
    let mut t = Term::new(60, 19);
    t.feed(b"/ $ ls\r\nfoo.txt  bar.txt  foo.rs\r\nsee http://example.com/x for the foo docs\r\n/ $ ");
    t
}

fn found(term: &Term, needle: &str) -> Find {
    let mut find = Find { text: needle.to_string(), caret: needle.chars().count(), ..Find::default() };
    find.restart();
    while !find.search.update(term, 4000, term.screen_top()) {}
    find
}

fn desktop(dark: bool) -> lazyterm::Rgb {
    scheme_by_name("desktop", dark).bg
}

#[test]
fn the_context_menu_is_a_panel_of_entries_at_the_pointer() {
    for (dark, scale, name) in [(true, 1, "term-menu-dark.png"), (false, 1, "term-menu-light.png"), (true, 2, "term-menu-2x.png")] {
        let shot = Shot {
            dark,
            dpi: 96 * scale as u32,
            chrome: |_| {
                let mut menu = Menu::new((60, 40), true, Some("http://example.com/x".to_string()));
                menu.selected = Some(1);
                Chrome { menu: Some(menu), ..Chrome::default() }
            },
            ..Shot::default()
        };
        let image = shoot(text_term(), &shot, name);
        let menu = Menu::new((60, 40), true, Some(String::new()));
        let laid = menu::layout(&menu, (640 * scale, 400 * scale), scale);
        let scheme = scheme_by_name("desktop", dark);
        // The panel is lighter than the window behind it, with an edge.
        let inside = image.pixel(laid.panel.left + 4 * scale, laid.panel.bottom - 4 * scale);
        assert_ne!(inside, scheme.bg, "{name}: the panel covers the grid");
        let edge = image.pixel(laid.panel.left, laid.panel.top + 20 * scale);
        assert_ne!(edge, inside, "{name}: an edge");
        // The selected row (Paste) is tinted; Copy (enabled here) is not.
        let paste = laid.rows[1];
        let copy = laid.rows[0];
        let sel = image.pixel(paste.right - 3 * scale, paste.top + 2 * scale);
        let plain = image.pixel(copy.right - 3 * scale, copy.top + 2 * scale);
        assert_ne!(sel, plain, "{name}: the selected entry shows");
        assert!(laid.panel.right <= 640 * scale && laid.panel.bottom <= 400 * scale);
    }
}

#[test]
fn a_menu_near_the_corner_stays_inside_the_window() {
    let shot = Shot {
        chrome: |_| Chrome { menu: Some(Menu::new((635, 395), false, None)), ..Chrome::default() },
        ..Shot::default()
    };
    let image = shoot(text_term(), &shot, "term-menu-corner.png");
    let laid = menu::layout(&Menu::new((635, 395), false, None), (640, 400), 1);
    assert!(laid.panel.right <= 640 && laid.panel.bottom <= 400);
    assert_ne!(image.pixel(laid.panel.left + 4, laid.panel.top + 4), desktop(true));
}

#[test]
fn find_matches_are_highlighted_and_the_current_one_is_stronger() {
    for (dark, name) in [(true, "term-find-dark.png"), (false, "term-find-light.png")] {
        let shot = Shot {
            dark,
            chrome: |term| {
                let mut find = found(term, "foo");
                find.search.next();
                Chrome { find: Some(find), ..Chrome::default() }
            },
            ..Shot::default()
        };
        let image = shoot(text_term(), &shot, name);
        let t = text_term();
        let find = found(&t, "foo");
        let scheme = scheme_by_name("desktop", dark);
        let matches = find.search.matches().to_vec();
        assert_eq!(matches.len(), 3, "foo.txt, foo.rs and the one in the sentence");
        // The first match is the current one after next() from an unset state
        // (index 0); the second is an ordinary match.
        let cell = |m: &lazyterm::Match| {
            let row = (m.start.line - t.screen_top()) as usize;
            image.corner(m.start.col, row)
        };
        let current = cell(&matches[0]);
        let other = cell(&matches[1]);
        assert_ne!(current, scheme.bg, "{name}: the current match is highlighted");
        assert_ne!(other, scheme.bg, "{name}: other matches are too");
        assert_ne!(current, other, "{name}: the current one is stronger");
        // Text that does not match is untouched.
        assert_eq!(image.corner(30, 1), scheme.bg, "{name}: no highlight elsewhere");
        // The bar is along the bottom: its field and buttons are drawn.
        let layout = Layout { width: 640, height: 400, pad: 10, line_h: 20, cell_w: 9, visible: 19 };
        let r = crate::findbar::rects(&layout);
        assert_ne!(image.pixel(r.bar.left + 2, r.bar.top + 5), scheme.bg, "{name}: the bar");
        assert_ne!(image.pixel(r.next.left + 2, r.next.top + 2), image.pixel(r.field.left + 2, r.field.top + 2));
    }
}

#[test]
fn a_match_across_wrapped_rows_is_highlighted_on_both() {
    let mut t = Term::new(20, 10);
    t.feed(b"abcdefghijklmnopqrstuvwxyz0123456789\r\n");
    let shot = Shot {
        chrome: |term| Chrome { find: Some(found(term, "stuvwx")), ..Chrome::default() },
        ..Shot::default()
    };
    let image = shoot(t, &shot, "term-find-wrapped.png");
    let scheme = scheme_by_name("desktop", true);
    // "stuvwx": "st" end the first row (cols 18-19), "uvwx" start the second.
    assert_ne!(image.corner(18, 0), scheme.bg);
    assert_ne!(image.corner(19, 0), scheme.bg);
    assert_ne!(image.corner(0, 1), scheme.bg);
    assert_ne!(image.corner(3, 1), scheme.bg);
    assert_eq!(image.corner(4, 1), scheme.bg);
}

#[test]
fn the_hovered_link_is_underlined() {
    let t = text_term();
    let hit = t.link_hit_at(t.screen_top() + 2, 10).expect("a link on the third row");
    let range = (hit.start, hit.end);
    let shot = Shot {
        chrome: |_| Chrome::default(),
        ..Shot::default()
    };
    let _ = shoot(text_term(), &shot, "term-link-plain.png");
    let hover = Shot {
        chrome: |term| {
            let hit = term.link_hit_at(term.screen_top() + 2, 10).unwrap();
            Chrome { hover: Some((hit.start, hit.end)), ..Chrome::default() }
        },
        ..Shot::default()
    };
    let image = shoot(text_term(), &hover, "term-link-hover.png");
    let fg = scheme_by_name("desktop", true).fg;
    let m = image.m;
    let (x, y) = image.origin(range.0.col + 3, 2);
    assert_eq!(image.pixel(x + 2, y + m.line_h - 2 * m.light), fg, "an underline under the link");
    let (x, y) = image.origin(range.1.col + 2, 2);
    assert_ne!(image.pixel(x + 2, y + m.line_h - 2 * m.light), fg, "and not past its end");
    let (x, y) = image.origin(range.0.col.saturating_sub(2), 2);
    assert_ne!(image.pixel(x + 1, y + m.line_h - 2 * m.light), fg, "or before it");
    let _ = Point::new(0, 0);
}

#[test]
fn the_preferences_panel_shows_every_control() {
    for (dark, scale, name) in [(true, 1, "term-prefs-dark.png"), (false, 1, "term-prefs-light.png"), (true, 2, "term-prefs-2x.png")] {
        let shot = Shot {
            dark,
            dpi: 96 * scale as u32,
            chrome: |_| {
                let families = vec!["JetBrains Mono".to_string(), "Liberation Mono".to_string()];
                Chrome { prefs: Some(Prefs::new(&Settings::default(), families)), ..Chrome::default() }
            },
            ..Shot::default()
        };
        let image = shoot(text_term(), &shot, name);
        let prefs = Prefs::new(&Settings::default(), vec!["JetBrains Mono".into(), "Liberation Mono".into()]);
        let laid = crate::prefs::layout(&prefs, (640 * scale, 400 * scale), scale);
        let scheme = scheme_by_name("desktop", dark);
        assert_ne!(image.pixel(laid.panel.left + 4 * scale, laid.panel.top + 40 * scale), scheme.bg, "{name}: the panel");
        // Each scheme swatch shows its own background.
        for (part, rect, label) in &laid.parts {
            if let crate::prefs::Part::Scheme(i) = part {
                let want = scheme_by_name(NAMES[*i], dark).bg;
                assert_eq!(image.pixel(rect.left + 2 * scale, rect.bottom - 3 * scale), want, "{name}: {label}");
            }
        }
        // The current family is highlighted in the list.
        let family = laid.parts.iter().find(|(p, _, _)| matches!(p, crate::prefs::Part::Family(n) if n == "JetBrains Mono")).unwrap();
        let other = laid.parts.iter().find(|(p, _, _)| matches!(p, crate::prefs::Part::Family(n) if n == "Liberation Mono")).unwrap();
        assert_ne!(image.pixel(family.1.right - 4 * scale, family.1.top + 2 * scale), image.pixel(other.1.right - 4 * scale, other.1.top + 2 * scale), "{name}: the selection");
        // The grid shows through nowhere inside the panel.
        assert_ne!(image.pixel(laid.panel.right - 4 * scale, laid.panel.bottom - 4 * scale), scheme.bg);
    }
}

#[test]
fn the_bell_flashes_a_frame_in_the_cursor_colour() {
    let shot = Shot { chrome: |_| Chrome { flash: true, ..Chrome::default() }, ..Shot::default() };
    let image = shoot(text_term(), &shot, "term-bell-flash.png");
    let cursor = scheme_by_name("desktop", true).cursor;
    assert_eq!(image.pixel(1, 200), cursor);
    assert_eq!(image.pixel(638, 200), cursor);
    assert_eq!(image.pixel(300, 1), cursor);
    assert_eq!(image.pixel(300, 398), cursor);
    assert_eq!(image.pixel(300, 300), desktop(true), "the middle is untouched");
}

#[test]
fn the_link_and_close_prompts_ask_in_the_bar() {
    for (kind, name) in [(0, "term-prompt-link.png"), (1, "term-prompt-close.png")] {
        let shot = Shot {
            chrome: if kind == 0 {
                (|_| Chrome { prompt: Some(Prompt::link("http://example.com/some/long/path?x=1")), ..Chrome::default() }) as fn(&Term) -> Chrome
            } else {
                (|_| Chrome { prompt: Some(Prompt::close()), ..Chrome::default() }) as fn(&Term) -> Chrome
            },
            ..Shot::default()
        };
        let image = shoot(text_term(), &shot, name);
        let layout = Layout { width: 640, height: 400, pad: 10, line_h: 20, cell_w: 9, visible: 19 };
        let bar = crate::geometry::confirm_rects(&layout);
        let scheme = scheme_by_name("desktop", true);
        let button = scheme.bg.lerp(scheme.fg, 0.32);
        assert_eq!(image.pixel(bar.paste.left + 1, bar.paste.top + 1), button, "{name}: the accepting button");
        assert_eq!(image.pixel(bar.cancel.left + 1, bar.cancel.top + 1), button, "{name}: Cancel");
        assert_eq!(image.pixel(3, bar.bar.top), scheme.cursor, "{name}: the edge");
    }
    let _ = (Item::Copy, Search::new("", true));
}
