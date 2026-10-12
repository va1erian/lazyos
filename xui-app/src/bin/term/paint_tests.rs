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
    xui_canvas::add_font(xui_app::font::MONO_BYTES.to_vec());
    catalog.add(&parse(xui_app::font::MONO_BYTES).unwrap());
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
    pending: Option<usize>,
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
            pending: None,
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
            let mut look = Look::new(
                metrics,
                setup.family,
                catalog.styles(setup.family).unwrap_or_default(),
            );
            look.cursor_shape = setup.shape;
            look.focused = setup.focused;
            look.cursor_on = setup.cursor_on;
            look.bold_is_bright = setup.bold_is_bright;
            let accent = theme.accent;
            let draw = move |canvas: &mut dyn xui_core::Canvas| {
                let scene = Scene {
                    term: &term,
                    look: &look,
                    offset: setup.offset,
                    pending: setup.pending,
                    accent: Rgb(accent.r, accent.g, accent.b),
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
    term.feed(b"\x1b[1mHHHH\x1b[0m HHHH");
    let jetbrains = shoot(term, &Shot::default(), "term-bold-jetbrains.png");
    let bg = scheme_by_name("desktop", true).bg;
    let count = |s: &Shown, col: usize| {
        let (x, y) = s.origin(col, 0);
        (0..s.m.line_h)
            .flat_map(|py| (0..4 * s.m.cell_w).map(move |px| (px, py)))
            .filter(|&(px, py)| s.pixel(x + px, y + py) != bg)
            .count()
    };
    assert!(count(&jetbrains, 0) > count(&jetbrains, 5), "overstrike adds ink: {} vs {}", count(&jetbrains, 0), count(&jetbrains, 5));
    let mut term = Term::new(80, 19);
    term.feed(b"\x1b[1mHHHH\x1b[0m HHHH\r\n\x1b[3mHHHH\x1b[0m HHHH");
    let shot = Shot { family: "Liberation Mono", ..Shot::default() };
    let liberation = shoot(term, &shot, "term-bold-italic-liberation.png");
    assert!(count(&liberation, 0) > count(&liberation, 5), "the real bold face is heavier");
}

#[test]
fn box_drawing_joins_across_cells_at_every_size() {
    const TABLE: &str = "\u{250c}\u{2500}\u{252c}\u{2500}\u{2510}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{251c}\u{2500}\u{253c}\u{2500}\u{2524}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}\r\n\u{2554}\u{2550}\u{2566}\u{2550}\u{2557}\r\n\u{2551} \u{2551} \u{2551}\r\n\u{255a}\u{2550}\u{2569}\u{2550}\u{255d}\r\n\u{2501}\u{2503}\u{250f}\u{2513}\u{2517}\u{251b}\u{2588}\u{2593}\u{2592}\u{2591}\u{2580}\u{2584}\u{258c}\u{2590}\u{2596}\u{259f}";
    for (size, dpi, name) in [(15.0, 96, "term-box-15.png"), (22.0, 96, "term-box-22.png"), (15.0, 192, "term-box-2x.png"), (11.0, 96, "term-box-11.png")] {
        let mut term = Term::new(80, 19);
        term.feed(TABLE.as_bytes());
        let shot = Shot { size, dpi, ..Shot::default() };
        let image = shoot(term, &shot, name);
        let scheme = scheme_by_name("desktop", true);
        let m = image.m;
        // The horizontal line of the top row, through every column from the
        // corner to the corner, is one unbroken run of foreground pixels.
        let (x0, y0) = image.origin(0, 0);
        let y = y0 + m.line_h / 2;
        for x in x0 + m.cell_w / 2..x0 + 4 * m.cell_w + m.cell_w / 2 {
            assert_eq!(image.pixel(x, y), scheme.fg, "{name}: top line at x={x}");
        }
        // The first column's vertical line runs from the top corner to the
        // bottom corner without a gap, through the tees and the cross.
        let x = x0 + m.cell_w / 2;
        for row_y in y..y0 + 4 * m.line_h + m.line_h / 2 {
            assert_eq!(image.pixel(x, row_y), scheme.fg, "{name}: left line at y={row_y}");
        }
        // A full block fills its cell edge to edge.
        let (bx, by) = image.origin(6, 8);
        for (dx, dy) in [(0, 0), (m.cell_w - 1, 0), (0, m.line_h - 1), (m.cell_w - 1, m.line_h - 1)] {
            assert_eq!(image.pixel(bx + dx, by + dy), scheme.fg, "{name}: full block corner");
        }
        // The shades darken from dark (U+2593) to light (U+2591).
        let shade = |col| {
            let (x, y) = image.origin(col, 8);
            image.pixel(x + 2, y + 2).0
        };
        assert!(shade(7) > shade(8) && shade(8) > shade(9), "{name}: shades in order");
    }
}

#[test]
fn wide_and_fallback_glyphs_do_not_shift_the_columns_after_them() {
    let mut plain = Term::new(80, 19);
    plain.feed(b"a  cd");
    let mut wide = Term::new(80, 19);
    wide.feed("a\u{4e2d}cd".as_bytes());
    let mut accented = Term::new(80, 19);
    accented.feed("a\u{e9} cd".as_bytes());
    let bg = scheme_by_name("desktop", true).bg;
    let shot = Shot::default();
    let (plain, wide, accented) = (shoot(plain, &shot, "term-columns-plain.png"), shoot(wide, &shot, "term-columns-wide.png"), shoot(accented, &shot, "term-columns-fallback.png"));
    // "c" and "d" are in the same cells (4 and 5; 4 and 5) in every row.
    for image in [&plain, &wide, &accented] {
        assert!(image.has_ink(3, 0, 1, bg) && image.has_ink(4, 0, 1, bg), "c and d keep their cells");
    }
    let cell_ink = |s: &Shown, col| {
        let (x, y) = s.origin(col, 0);
        (0..s.m.line_h)
            .flat_map(|py| (0..s.m.cell_w).map(move |px| (px, py)))
            .filter(|&(px, py)| s.pixel(x + px, y + py) != bg)
            .count()
    };
    for col in [3, 4] {
        assert_eq!(cell_ink(&plain, col), cell_ink(&wide, col), "wide: column {col}");
        assert_eq!(cell_ink(&plain, col), cell_ink(&accented, col), "accent: column {col}");
    }
}

#[test]
fn a_fractional_advance_still_lands_every_glyph_in_its_cell() {
    // 17 px JetBrains Mono is 10.2 px a glyph: the cell is 10, the run is not
    // drawn as a string.
    let mut term = Term::new(80, 19);
    term.feed(b"MMMMMMMMMMMMMMMMMMMMMMMMMMMMMMMM\r\n");
    let shot = Shot { size: 17.0, ..Shot::default() };
    let image = shoot(term, &shot, "term-fractional-17.png");
    let bg = scheme_by_name("desktop", true).bg;
    assert!(!image.m.exact, "10.2 px advances are not exact");
    // The last M's ink is inside its own cell, not 6 px to the right of it.
    let last = 31;
    assert!(image.has_ink(last, 0, 1, bg));
    let (x, y) = image.origin(last + 1, 0);
    let past = (0..image.m.line_h).any(|py| (0..image.m.cell_w).any(|px| image.pixel(x + px, y + py) != bg));
    assert!(!past, "nothing beyond the last cell");
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
fn zoom_and_scale_change_the_grid_not_the_design() {
    for (size, dpi, name) in [(15.0, 96, "term-size-15.png"), (24.0, 96, "term-size-24.png"), (11.0, 96, "term-size-11.png"), (15.0, 192, "term-2x.png")] {
        let mut term = Term::new(80, 40);
        term.feed(b"size and scale: Hello, World\r\n0123456789");
        let shot = Shot { size, dpi, ..Shot::default() };
        let image = shoot(term, &shot, name);
        let m = image.m;
        assert_eq!(m.scale, (dpi / 96) as i32);
        assert!(m.cell_w >= (0.55 * size * m.scale as f32) as i32 - 1, "{name}: the advance of M, not 0.6 em guessed");
        assert!(m.line_h >= (size * m.scale as f32) as i32);
        // The cursor sits after the last digit, exactly where the metrics say.
        let scheme = scheme_by_name("desktop", true);
        let (x, y) = image.origin(10, 1);
        assert_eq!(image.pixel(x + 1, y + m.line_h - 1), scheme.cursor, "{name}: cursor cell");
    }
}

#[test]
fn line_spacing_sets_the_row_height() {
    let mut heights = Vec::new();
    for (spacing, name) in [(100, "term-spacing-100.png"), (130, "term-spacing-130.png"), (180, "term-spacing-180.png")] {
        let mut term = Term::new(80, 40);
        term.feed(b"line one\r\nline two\r\nline three");
        let image = shoot(term, &Shot { spacing, ..Shot::default() }, name);
        heights.push(image.m.line_h);
    }
    assert!(heights[0] < heights[1] && heights[1] < heights[2], "{heights:?}");
}

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
    let attr = |flags: u16, fg: lazyterm::Color, bg: lazyterm::Color| lazyterm::Attr { fg, bg, flags: lazyterm::Flags(flags) };
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
