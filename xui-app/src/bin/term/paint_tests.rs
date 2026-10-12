//! Offscreen snapshots of the painter through xui's headless renderer
//! (`xui-app/target/snapshots/term-*.png`, for a human to look at). They link
//! the whole app library, so they run on Linux (WSL on Windows).

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use lazyterm::font::{parse, Catalog};
use lazyterm::palette::{scheme_by_name, NAMES};
use lazyterm::settings::CursorShape;
use lazyterm::{Point, Rgb, SelectMode, Term};
use xui_canvas::snapshot::{render, Snapshot};
use xui_core::app::{App, Ui};
use xui_core::backend::{NodeKind, NodeSpec};
use xui_core::{Control, Image, Theme};

use super::super::geometry::confirm_rects;
use super::super::chrome::Chrome;
use super::super::prompt::Prompt;
use super::super::look::{measure, Look};
use super::*;

const LIBERATION: [&[u8]; 4] = [
    include_bytes!("../../../../assets/fonts/liberation/LiberationMono-Regular.ttf"),
    include_bytes!("../../../../assets/fonts/liberation/LiberationMono-Bold.ttf"),
    include_bytes!("../../../../assets/fonts/liberation/LiberationMono-Italic.ttf"),
    include_bytes!("../../../../assets/fonts/liberation/LiberationMono-BoldItalic.ttf"),
];

struct Host {
    _root: Control<()>,
}

impl App for Host {
    type Msg = ();

    fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
}

/// Register the faces as `main` does. The registry is per thread and each
/// test runs on its own thread, so every render registers again.
fn fonts() -> Catalog {
    let mut catalog = Catalog::default();
    for face in std::iter::once(xui_app::font::MONO_BYTES).chain(xui_app::font::MONO_STYLES) {
        xui_canvas::add_font(face.to_vec());
        catalog.add(&parse(face).unwrap());
    }
    for face in LIBERATION {
        xui_canvas::add_font(face.to_vec());
        catalog.add(&parse(face).unwrap());
    }
    xui_canvas::set_default_family(xui_app::font::MONO_FAMILY);
    catalog
}

/// How one snapshot is set up.
#[derive(Clone)]
struct Shot {
    dark: bool,
    scheme: &'static str,
    family: &'static str,
    size: f32,
    spacing: u32,
    dpi: u32,
    shape: CursorShape,
    focused: bool,
    cursor_on: bool,
    bold_is_bright: bool,
    offset: usize,
    /// Builds the layers over the grid (menu, find bar, prompt, ...).
    chrome: fn(&Term) -> Chrome,
    no_bold: bool,
}

impl Default for Shot {
    fn default() -> Shot {
        Shot {
            dark: true,
            scheme: "desktop",
            family: "JetBrains Mono",
            size: 15.0,
            spacing: 130,
            dpi: 96,
            shape: CursorShape::Block,
            focused: true,
            cursor_on: true,
            bold_is_bright: false,
            offset: 0,
            chrome: |_| Chrome::default(),
            no_bold: false,
        }
    }
}

/// What a snapshot gave back: the image and the metrics it was drawn with.
struct Shown {
    image: Image,
    m: Metrics,
}

use super::super::metrics::Metrics;

/// Render `term` as `shot` says, with the painter run `times` times in one
/// frame (the timing test) and the picture saved as `name`.
fn render_with(mut term: Term, shot: &Shot, times: u32, name: &str) -> (Shown, f64) {
    let catalog = fonts();
    let theme = if shot.dark { Theme::dark() } else { Theme::light() };
    if shot.scheme != "desktop" || !shot.dark {
        term.set_scheme(scheme_by_name(shot.scheme, shot.dark));
    }
    let term = Rc::new(term);
    let result = Rc::new(std::cell::Cell::new((None::<Metrics>, 0f64)));
    let out = Rc::clone(&result);
    let setup = shot.clone();
    let image = render(
        Snapshot::new(xui_core::Dip(640.0), xui_core::Dip(400.0))
            .theme(theme)
            .dpi(shot.dpi),
        move |ui| {
            let root = Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
                .expect("root node");
            let metrics = measure(ui, setup.family, setup.size, setup.spacing);
            let mut styles = catalog.styles(setup.family).unwrap_or_default();
            // A family that lacks its bold face: overstruck.
            styles.bold &= !setup.no_bold;
            let mut look = Look::new(metrics, setup.family, styles);
            look.cursor_shape = setup.shape;
            look.focused = setup.focused;
            look.cursor_on = setup.cursor_on;
            look.bold_is_bright = setup.bold_is_bright;
            let accent = theme.accent;
            let draw = move |canvas: &mut dyn xui_core::Canvas| {
                let chrome = (setup.chrome)(&term);
                let scene = Scene {
                    term: &term,
                    look: &look,
                    offset: setup.offset,
                    accent: Rgb(accent.r, accent.g, accent.b),
                    chrome: &chrome,
                    dark: setup.dark,
                };
                let start = Instant::now();
                for _ in 0..times {
                    paint(canvas, &scene, None);
                }
                out.set((Some(metrics), start.elapsed().as_secs_f64() * 1000.0 / f64::from(times)));
            };
            root.set_painter(Rc::new(draw));
            Host { _root: root }
        },
    )
    .expect("the headless render");
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
    let (metrics, ms) = result.take();
    (
        Shown {
            image,
            m: metrics.expect("painted"),
        },
        ms,
    )
}

fn shoot(term: Term, shot: &Shot, name: &str) -> Shown {
    render_with(term, shot, 1, name).0
}

impl Shown {
    fn pixel(&self, x: i32, y: i32) -> Rgb {
        let [r, g, b, _] = self.image.pixel(x as u32, y as u32).expect("inside the image");
        Rgb(r, g, b)
    }

    /// The top-left pixel of cell (`col`, `row`).
    fn origin(&self, col: usize, row: usize) -> (i32, i32) {
        (
            self.m.pad + col as i32 * self.m.cell_w,
            self.m.pad + row as i32 * self.m.line_h,
        )
    }

    /// A pixel inside cell (`col`, `row`) near its bottom-left, where no glyph
    /// reaches: its background.
    fn corner(&self, col: usize, row: usize) -> Rgb {
        let (x, y) = self.origin(col, row);
        self.pixel(x + 1, y + self.m.line_h - 1)
    }

    /// Whether any pixel of cell (`col`, `row`) differs from `bg`.
    fn has_ink(&self, col: usize, row: usize, cells: usize, bg: Rgb) -> bool {
        let (x, y) = self.origin(col, row);
        (y..y + self.m.line_h)
            .any(|py| (x..x + cells as i32 * self.m.cell_w).any(|px| self.pixel(px, py) != bg))
    }
}

fn ansi(dark: bool, i: usize) -> Rgb {
    scheme_by_name("desktop", dark).ansi[i]
}

fn shell() -> Term {
    let mut term = Term::new(80, 19);
    term.feed(b"/ # echo hi\r\nhi\r\n/ # ls\r\nbin  etc  home\r\n/ # ");
    term
}

#[test]
fn the_screen_paints_text_and_a_cursor_in_both_themes() {
    for (dark, tag) in [(true, "dark"), (false, "light")] {
        let shot = Shot { dark, ..Shot::default() };
        let image = shoot(shell(), &shot, &format!("term-shell-{tag}.png"));
        let scheme = scheme_by_name("desktop", dark);
        assert_eq!(image.pixel(5, 5), scheme.bg);
        assert_eq!(image.pixel(630, 390), scheme.bg);
        assert!(image.has_ink(0, 0, 11, scheme.bg), "{tag}: text");
        assert!(!image.has_ink(0, 10, 11, scheme.bg), "{tag}: blank row");
        let (x, y) = image.origin(4, 4);
        assert_eq!(image.pixel(x + 2, y + image.m.line_h - 2), scheme.cursor, "{tag}: the cursor");
    }
}

#[test]
fn every_scheme_paints_its_background_foreground_and_palette() {
    let mut sample = Term::new(80, 19);
    sample.feed(b"scheme sample\r\n");
    for i in 0..16 {
        sample.feed(format!("\x1b[4{}m  \x1b[0m", i % 8).as_bytes());
        if i == 7 {
            sample.feed(b"\r\n");
        }
    }
    sample.feed(b"\r\n");
    for i in 0..16 {
        sample.feed(format!("\x1b[48;5;{i}m  \x1b[0m").as_bytes());
    }
    sample.feed(b"\r\n\x1b[1;31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[7minverse\x1b[0m");
    for name in NAMES {
        for dark in [true, false] {
            if name != "desktop" && !dark {
                continue;
            }
            let scheme = scheme_by_name(name, dark);
            let mut term = Term::new(80, 19);
            term.feed(b"x");
            let shot = Shot { dark, scheme: name, ..Shot::default() };
            let mut copy = Term::new(80, 19);
            copy.feed(b"scheme sample\r\n");
            for i in 0..16 {
                copy.feed(format!("\x1b[48;5;{i}m  \x1b[0m").as_bytes());
                if i == 7 {
                    copy.feed(b"\r\n");
                }
            }
            copy.feed(b"\r\n\x1b[1;31mred\x1b[0m \x1b[32mgreen\x1b[0m \x1b[7minverse\x1b[0m");
            term.feed(b"");
            let tag = format!("{name}{}", if name == "desktop" { if dark { "-dark" } else { "-light" } } else { "" });
            let image = shoot(copy, &shot, &format!("term-scheme-{tag}.png"));
            assert_eq!(image.pixel(8, 8), scheme.bg, "{tag}: the background");
            for i in 0..8 {
                assert_eq!(image.corner(2 * i, 1), scheme.ansi[i], "{tag}: colour {i}");
                assert_eq!(image.corner(2 * i, 2), scheme.ansi[8 + i], "{tag}: colour {}", 8 + i);
            }
            // "inverse" swaps the scheme's own foreground and background.
            let (x, y) = image.origin(15, 3);
            assert_eq!(image.pixel(x + 1, y + image.m.line_h - 1), scheme.fg, "{tag}: inverse");
        }
    }
    drop(sample);
}

#[test]
fn the_256_colour_cube_and_true_colour_reach_the_screen() {
    let mut term = Term::new(80, 19);
    for n in [16u8, 21, 196, 46, 231, 244] {
        term.feed(format!("\x1b[48;5;{n}m \x1b[0m").as_bytes());
    }
    term.feed(b"\r\n");
    for step in 0..16u32 {
        let (r, g, b) = (step * 17, 255 - step * 17, 128);
        term.feed(format!("\x1b[48;2;{r};{g};{b}m \x1b[0m").as_bytes());
    }
    let image = shoot(term, &Shot::default(), "term-colors-256-rgb.png");
    let expected = [Rgb(0, 0, 0), Rgb(0, 0, 255), Rgb(255, 0, 0), Rgb(0, 255, 0), Rgb(255, 255, 255), Rgb(128, 128, 128)];
    for (col, rgb) in expected.into_iter().enumerate() {
        assert_eq!(image.corner(col, 0), rgb, "256-colour {col}");
    }
    assert_eq!(image.corner(0, 1), Rgb(0, 255, 128));
    assert_eq!(image.corner(15, 1), Rgb(255, 0, 128));
    assert_eq!(image.corner(8, 1), Rgb(136, 119, 128));
}

#[test]
fn attributes_are_painted() {
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[7minverse\x1b[0m \x1b[2mdim\x1b[0m \x1b[9mstrike\x1b[0m \x1b[8mhidden\x1b[0m");
    term.feed(b"\r\n\x1b[1;31mbold red\x1b[0m \x1b[91mbright red\x1b[0m");
    let shot = Shot { bold_is_bright: true, ..Shot::default() };
    let image = shoot(term, &shot, "term-attributes.png");
    let scheme = scheme_by_name("desktop", true);
    let m = image.m;
    // Underline: a row of foreground pixels under "underline" (columns 12-20).
    let (x, y) = image.origin(12, 0);
    assert_eq!(image.pixel(x + 3, y + m.line_h - 2 * m.light), scheme.fg, "underline");
    // Inverse: the cells take the foreground as a background.
    let (x, y) = image.origin(22, 0);
    assert_eq!(image.pixel(x + 1, y + m.line_h - 1), scheme.fg, "inverse background");
    // Strike: a line through the middle of "strike".
    let (x, y) = image.origin(34, 0);
    assert_eq!(image.pixel(x + 3, y + (m.line_h - m.light) / 2), scheme.fg, "strike");
    // Hidden draws nothing at all.
    assert!(!image.has_ink(41, 0, 6, scheme.bg), "hidden");
    // Bold red is the bright red when bold is bright; bright red is too.
    let bright = ansi(true, 9);
    let mut found = false;
    for px in 0..8 * m.cell_w {
        for py in 0..m.line_h {
            let (bx, by) = image.origin(0, 1);
            found |= image.pixel(bx + px, by + py) == bright;
        }
    }
    assert!(found, "bold red paints the bright red");
    // Dim is the foreground halfway to the background: no pixel is brighter.
    let dim = scheme.fg.lerp(scheme.bg, 0.5);
    let (dx, dy) = image.origin(29, 0);
    let ink: Vec<Rgb> = (0..m.line_h)
        .flat_map(|py| (0..3 * m.cell_w).map(move |px| (px, py)))
        .map(|(px, py)| image.pixel(dx + px, dy + py))
        .filter(|&p| p != scheme.bg)
        .collect();
    assert!(!ink.is_empty());
    assert!(ink.iter().all(|p| p.0 <= dim.0 + 2 && p.1 <= dim.1 + 2 && p.2 <= dim.2 + 2), "dim stays dim");
}

#[test]
fn bold_without_a_bold_face_is_overstruck_and_with_one_it_is_the_face() {
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b[1mHHHH\x1b[0m HHHH\r\n\x1b[3mHHHH\x1b[0m HHHH\r\n\x1b[1;3mHHHH\x1b[0m HHHH");
    let real = shoot(term, &Shot::default(), "term-jetbrains-faces.png");
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b[1mHHHH\x1b[0m HHHH");
    let jetbrains = shoot(term, &Shot { no_bold: true, ..Shot::default() }, "term-bold-jetbrains.png");
    let bg = scheme_by_name("desktop", true).bg;
    let count = |s: &Shown, col: usize| {
        let (x, y) = s.origin(col, 0);
        (0..s.m.line_h)
            .flat_map(|py| (0..4 * s.m.cell_w).map(move |px| (px, py)))
            .filter(|&(px, py)| s.pixel(x + px, y + py) != bg)
            .count()
    };
    assert!(count(&jetbrains, 0) > count(&jetbrains, 5), "overstrike adds ink: {} vs {}", count(&jetbrains, 0), count(&jetbrains, 5));
    // JetBrains Mono has its real faces: bold is heavier, and the italic and
    // bold italic rows are drawn from their own faces, not the Regular's.
    assert!(count(&real, 0) > count(&real, 5), "the real bold face is heavier");
    let row = |s: &Shown, r: usize| {
        let (x, y) = s.origin(0, r);
        (0..s.m.line_h)
            .flat_map(|py| (0..4 * s.m.cell_w).map(move |px| (px, py)))
            .filter(|&(px, py)| s.pixel(x + px, y + py) != bg)
            .collect::<Vec<_>>()
    };
    assert_ne!(row(&real, 1), row(&real, 0), "italic differs from the regular");
    assert_ne!(row(&real, 2), row(&real, 1), "bold italic differs from italic");
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b[1mHHHH\x1b[0m HHHH\r\n\x1b[3mHHHH\x1b[0m HHHH");
    let shot = Shot { family: "Liberation Mono", ..Shot::default() };
    let liberation = shoot(term, &shot, "term-bold-italic-liberation.png");
    assert!(count(&liberation, 0) > count(&liberation, 5), "the real bold face is heavier");
}

#[test]
fn the_cursor_shapes_and_the_unfocused_outline() {
    let mut term = Term::new(80, 19);
    term.feed(b"abc");
    for (shape, name) in [(CursorShape::Block, "block"), (CursorShape::Underline, "underline"), (CursorShape::Bar, "bar")] {
        let shot = Shot { shape, ..Shot::default() };
        let mut t = Term::new(80, 19);
        t.feed(b"abc");
        let image = shoot(t, &shot, &format!("term-cursor-{name}.png"));
        let scheme = scheme_by_name("desktop", true);
        let (x, y) = image.origin(3, 0);
        let m = image.m;
        match shape {
            CursorShape::Block => {
                assert_eq!(image.pixel(x + 1, y + 1), scheme.cursor);
                assert_eq!(image.pixel(x + m.cell_w - 2, y + m.line_h - 2), scheme.cursor);
            }
            CursorShape::Underline => {
                assert_eq!(image.pixel(x + 2, y + m.line_h - 1), scheme.cursor);
                assert_eq!(image.pixel(x + 2, y + 2), scheme.bg, "only the bottom");
            }
            CursorShape::Bar => {
                assert_eq!(image.pixel(x, y + m.line_h / 2), scheme.cursor);
                assert_eq!(image.pixel(x + m.cell_w - 2, y + m.line_h / 2), scheme.bg, "only the left edge");
            }
        }
    }
    let scheme = scheme_by_name("desktop", true);
    let unfocused = shoot(term, &Shot { focused: false, ..Shot::default() }, "term-cursor-unfocused.png");
    let (x, y) = unfocused.origin(3, 0);
    let m = unfocused.m;
    assert_eq!(unfocused.pixel(x, y + m.line_h / 2), scheme.cursor, "the outline's left edge");
    assert_eq!(unfocused.pixel(x + m.cell_w / 2, y + m.line_h / 2), scheme.bg, "hollow inside");
    // The blink's off phase: nothing.
    let mut term = Term::new(80, 19);
    term.feed(b"abc");
    let off = shoot(term, &Shot { cursor_on: false, ..Shot::default() }, "term-cursor-blink-off.png");
    assert_eq!(off.pixel(x + 1, y + 1), scheme.bg);
}

#[test]
fn a_block_cursor_redraws_the_character_under_it_in_the_background_colour() {
    let mut term = Term::new(80, 19);
    term.feed(b"abcM\x1b[1D");
    let image = shoot(term, &Shot::default(), "term-cursor-over-glyph.png");
    let scheme = scheme_by_name("desktop", true);
    let (x, y) = image.origin(3, 0);
    let m = image.m;
    let ink = (0..m.line_h)
        .flat_map(|py| (0..m.cell_w).map(move |px| (px, py)))
        .filter(|&(px, py)| image.pixel(x + px, y + py) != scheme.cursor)
        .count();
    assert!(ink > 8, "the M is cut out of the block in the background colour: {ink}");
}

#[test]
fn a_program_changed_palette_is_what_is_painted() {
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b]4;1;rgb:12/34/56\x07\x1b[41m \x1b[0m\x1b]11;#102030\x07");
    let image = shoot(term, &Shot::default(), "term-osc-palette.png");
    assert_eq!(image.corner(0, 0), Rgb(0x12, 0x34, 0x56));
    assert_eq!(image.pixel(300, 300), Rgb(0x10, 0x20, 0x30), "the new default background");
}

#[test]
fn the_cell_colours_follow_inverse_dim_hidden_and_bold_is_bright() {
    let palette = lazyterm::Palette::new(scheme_by_name("desktop", true));
    let attr = |flags: u16, fg: lazyterm::Color, bg: lazyterm::Color| lazyterm::Attr { fg, bg, flags: lazyterm::Flags(flags), link: 0 };
    use lazyterm::{Color, Flags};
    let (fg, bg) = cell_colors(&palette, attr(Flags::INVERSE, Color::Default, Color::Default), false);
    assert_eq!((fg, bg), (palette.bg(), palette.fg()));
    let (fg, bg) = cell_colors(&palette, attr(Flags::DIM, Color::Default, Color::Default), false);
    assert_eq!(fg, palette.fg().lerp(palette.bg(), 0.5));
    assert_eq!(bg, palette.bg());
    let (fg, bg) = cell_colors(&palette, attr(Flags::HIDDEN, Color::Indexed(1), Color::Indexed(4)), false);
    assert_eq!(fg, bg, "hidden text is its background");
    let (fg, _) = cell_colors(&palette, attr(Flags::BOLD, Color::Indexed(2), Color::Default), true);
    assert_eq!(fg, palette.get(10));
    let (fg, _) = cell_colors(&palette, attr(Flags::BOLD, Color::Indexed(2), Color::Default), false);
    assert_eq!(fg, palette.get(2));
}

/// Paint time per full frame on the offscreen software canvas (host side, so
/// a floor for QEMU, not a figure for it): `--ignored --nocapture`.
#[test]
#[ignore]
fn bench_full_frame() {
    let mut term = Term::new(80, 19);
    for row in 0..19 {
        term.feed(format!("\x1b[3{}mrow {row:02} the quick brown fox jumps over the lazy dog \x1b[1m0123456789\x1b[0m\r\n", 1 + row % 6).as_bytes());
    }
    term.feed("\u{250c}\u{2500}\u{2510}\u{2502}x\u{2502}\u{2514}\u{2500}\u{2518}".as_bytes());
    for (size, name) in [(15.0, "15 px (exact advance)"), (17.0, "17 px (per-glyph cells)")] {
        let shot = Shot { size, ..Shot::default() };
        let mut copy = Term::new(80, 19);
        copy.feed(b"x");
        let (_, ms) = render_with(replay(&term), &shot, 30, "term-bench.png");
        println!("PAINT:FRAME {name}: {ms:.2} ms per full frame");
        drop(copy);
    }
}

/// A copy of `term`'s screen as output bytes (the engine is not `Clone`).
fn replay(term: &Term) -> Term {
    let mut copy = Term::new(term.cols(), term.rows());
    for row in 0..term.rows() {
        copy.feed(format!("\x1b[{};1H{}", row + 1, term.row_text(row)).as_bytes());
    }
    copy
}


#[path = "paint_tests_chrome.rs"]
mod chrome_tests;
#[path = "paint_tests_geom.rs"]
mod geom;
#[path = "paint_tests_view.rs"]
mod view_tests;
