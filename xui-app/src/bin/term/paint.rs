//! The Terminal's painter: the engine's screen drawn in one colour, one text
//! run per row and a block-underline cursor (attributes are stored by the
//! engine but painted from T3 on).

use lazyterm::Term;
use xui_core::{Canvas, Color, Dip, Rect, TextStyle};

use super::geometry::confirm_rects;
use super::view::{
    first_row, intersect, line_number, max_offset, overlaps, thumb, view_line, Layout,
};

/// Text size and the monospace cell it implies (JetBrains Mono is 0.6 em wide).
pub const FONT: f32 = 15.0;
pub const CELL_W: f32 = FONT * 0.6;
pub const LINE_H: i32 = 20;
pub const PAD: i32 = 10;

/// Background, text and cursor colours for the desktop's dark and light modes.
pub struct Palette {
    pub bg: Color,
    pub fg: Color,
    pub cursor: Color,
    /// Behind selected text: readable against `fg` in this theme.
    pub select: Color,
}

/// Midnight's navy, a shade deeper than a window so the grid reads as a well.
pub const DARK: Palette = Palette {
    bg: Color::rgb(0x15, 0x19, 0x28),
    fg: Color::rgb(0xdc, 0xe1, 0xf0),
    cursor: Color::rgb(0x5f, 0xd0, 0x8f),
    select: Color::rgb(0x2f, 0x52, 0x8f),
};
pub const LIGHT: Palette = Palette {
    bg: Color::rgb(0xfb, 0xfb, 0xfb),
    fg: Color::rgb(0x1f, 0x23, 0x28),
    cursor: Color::rgb(0x09, 0x69, 0xda),
    select: Color::rgb(0xb4, 0xd5, 0xfe),
};

/// The characters of the first `cols` cells of a line (`None`: a blank slot);
/// the right half of a wide character is not a character.
fn line_string(cells: &[lazyterm::Cell], cols: usize) -> String {
    cells
        .iter()
        .take(cols)
        .filter(|cell| cell.is_glyph())
        .map(|cell| cell.ch)
        .collect()
}

/// Paint the view of the screen that fits the window, scrolled back `offset`
/// lines into the history, with a block cursor and, while scrolled back, a
/// thin scrollbar in the right gutter:
/// only the rows inside `damage` (window pixels; `None` paints them all),
/// since only the damage reaches the window's frame. Returns the layout, so
/// the next drain can turn changed rows into a rectangle.
/// The metrics are design pixels, times the UI scale (`canvas.dpi() / 96`)
/// on screen, so the cells grow with the text (docs/hidpi-plan.md).
pub fn paint(
    canvas: &mut dyn Canvas,
    palette: &Palette,
    term: &Term,
    offset: usize,
    pending: Option<usize>,
    damage: Option<Rect>,
) -> Layout {
    let bounds = canvas.bounds();
    let damage = intersect(damage.unwrap_or(bounds), bounds);
    canvas.fill_rect(damage, palette.bg);
    let scale = (canvas.dpi() / 96).max(1) as i32;
    let (pad, line_h, cell_w) = (PAD * scale, LINE_H * scale, CELL_W * scale as f32);
    let rows = term.rows();
    let visible_cols = ((bounds.width() - 2 * pad) as f32 / cell_w) as usize;
    let visible = (((bounds.height() - 2 * pad) / line_h).max(1) as usize).min(rows);
    let (cursor_row, cursor_col) = term.cursor();
    let first = first_row(cursor_row, visible, rows);
    let row_rect = |slot: usize| {
        let top = bounds.top + pad + slot as i32 * line_h;
        Rect::new(bounds.left, top, bounds.right, top + line_h)
    };
    for slot in 0..visible {
        if !overlaps(row_rect(slot), damage) {
            continue;
        }
        // The selection sits behind the text.
        if let Some((from, to)) = line_number(term, first, offset, slot)
            .and_then(|line| term.selection_span(line))
            .filter(|&(from, _)| from < visible_cols)
        {
            let top = bounds.top + pad + slot as i32 * line_h;
            let left = bounds.left + pad + (from as f32 * cell_w) as i32;
            let right = bounds.left + pad + ((to.min(visible_cols - 1) + 1) as f32 * cell_w) as i32;
            canvas.fill_rect(Rect::new(left, top, right, top + line_h), palette.select);
        }
        let Some(line) = view_line(term, first, offset, slot) else {
            continue;
        };
        let text = line_string(line.cells, visible_cols.min(term.cols()));
        let top = bounds.top + pad + slot as i32 * line_h;
        let rect = Rect::new(bounds.left + pad, top, bounds.right - pad, top + line_h);
        canvas.draw_text(&text, rect, &TextStyle::new(palette.fg, Dip(FONT)));
    }
    // The cursor is a screen row: it is in view only while its slot is.
    let slot = (cursor_row + offset).saturating_sub(first);
    if term.cursor_visible() && cursor_row >= first && slot < visible && cursor_col < visible_cols
    {
        let top = bounds.top + pad + slot as i32 * line_h;
        let left = bounds.left + pad + (cursor_col as f32 * cell_w) as i32;
        let rect = Rect::new(
            left,
            top + line_h - 3 * scale,
            left + cell_w as i32,
            top + line_h,
        );
        if overlaps(rect, damage) {
            canvas.fill_rect(rect, palette.cursor);
        }
    }
    let track_top = bounds.top + pad;
    let track_h = bounds.height() - 2 * pad;
    let min_h = 16 * scale;
    if let Some((top, bottom)) = thumb(track_top, track_h, max_offset(term, visible), offset, visible, min_h) {
        // A thin pill in the right gutter, between the background and the text.
        let (left, right) = (bounds.right - pad * 7 / 10, bounds.right - pad * 3 / 10);
        let rect = Rect::new(left, top, right, bottom);
        if overlaps(rect, damage) {
            canvas.fill_rounded_rect(rect, (right - left) as f32 / 2.0, palette.bg.lerp(palette.fg, 0.45));
        }
    }
    let laid = Layout {
        width: bounds.width(),
        height: bounds.height(),
        pad,
        line_h,
        visible,
    };
    if let Some(lines) = pending {
        paint_confirm(canvas, palette, &laid, lines, damage);
    }
    laid
}

/// The paste confirmation bar: the question, a Paste and a Cancel button
/// (Enter and Escape do the same).
fn paint_confirm(
    canvas: &mut dyn Canvas,
    palette: &Palette,
    layout: &Layout,
    lines: usize,
    damage: Rect,
) {
    let c = confirm_rects(layout);
    if !overlaps(c.bar, damage) {
        return;
    }
    let scale = (layout.pad / PAD).max(1);
    let style = TextStyle::new(palette.fg, Dip(FONT));
    canvas.fill_rect(c.bar, palette.bg.lerp(palette.fg, 0.16));
    canvas.fill_rect(Rect::new(c.bar.left, c.bar.top, c.bar.right, c.bar.top + scale), palette.cursor);
    let text = |canvas: &mut dyn Canvas, text: &str, rect: Rect| {
        let at = Rect::new(rect.left, rect.top + (rect.height() - LINE_H * scale) / 2, rect.right, rect.bottom);
        canvas.draw_text(text, at, &style);
    };
    let question = format!("Paste {lines} lines?");
    text(canvas, &question, Rect::new(c.bar.left + layout.pad, c.bar.top, c.paste.left, c.bar.bottom));
    for (rect, label) in [(c.paste, "Paste"), (c.cancel, "Cancel")] {
        canvas.fill_rect(rect, palette.bg.lerp(palette.fg, 0.32));
        text(canvas, label, Rect::new(rect.left + layout.pad, rect.top, rect.right, rect.bottom));
    }
}

/// Offscreen snapshots of the painter through xui's headless renderer
/// (`xui-app/target/snapshots/term-*.png`, for a human to look at). They link
/// the whole app library, so they run on Linux (WSL on Windows).
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::rc::Rc;

    use xui_canvas::snapshot::{render, Snapshot};
    use xui_core::app::{App, Ui};
    use xui_core::backend::{NodeKind, NodeSpec};
    use xui_core::{Control, Image, Theme};

    use super::*;

    struct Host {
        _root: Control<()>,
    }

    impl App for Host {
        type Msg = ();

        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    /// Register the mono face as `main` does. The registry is per thread and
    /// each test runs on its own thread, so every render registers again.
    fn fonts() {
        xui_canvas::add_font(xui_app::font::MONO_BYTES.to_vec());
        xui_canvas::set_default_family(xui_app::font::MONO_FAMILY);
    }

    /// `term` painted at `dpi` in `dark` or light, saved as `name`.
    fn shoot(term: Term, dark: bool, dpi: u32, name: &str) -> Image {
        shoot_at(term, 0, dark, dpi, name)
    }

    /// The same, scrolled back `offset` lines.
    fn shoot_at(term: Term, offset: usize, dark: bool, dpi: u32, name: &str) -> Image {
        shoot_full(term, offset, None, dark, dpi, name)
    }

    /// The same, with the paste confirmation bar up for `pending` lines.
    fn shoot_full(
        term: Term,
        offset: usize,
        pending: Option<usize>,
        dark: bool,
        dpi: u32,
        name: &str,
    ) -> Image {
        fonts();
        let (palette, theme) = if dark {
            (&DARK, Theme::dark())
        } else {
            (&LIGHT, Theme::light())
        };
        let term = Rc::new(term);
        let image = render(
            Snapshot::new(Dip(640.0), Dip(400.0)).theme(theme).dpi(dpi),
            |ui| {
                let root =
                    Control::new(ui, &NodeSpec::new(NodeKind::Custom, ui.client_rect()))
                        .expect("root node");
                root.set_painter(Rc::new(move |canvas| {
                    paint(canvas, palette, &term, offset, pending, None);
                }));
                Host { _root: root }
            },
        )
        .expect("the headless render");
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        image.save_png(dir.join(name)).unwrap();
        image
    }

    fn rgb(color: Color) -> [u8; 3] {
        [color.r, color.g, color.b]
    }

    fn pixel(image: &Image, x: i32, y: i32) -> [u8; 3] {
        let [r, g, b, _] = image.pixel(x as u32, y as u32).expect("inside the image");
        [r, g, b]
    }

    /// Whether any pixel in the box differs from the background.
    fn has_ink(image: &Image, bg: Color, (x0, y0, x1, y1): (i32, i32, i32, i32)) -> bool {
        (y0..y1).any(|y| (x0..x1).any(|x| pixel(image, x, y) != rgb(bg)))
    }

    fn shell() -> Term {
        let mut term = Term::new(80, 24);
        term.feed(b"/ # echo hi\r\nhi\r\n/ # ls\r\nbin  etc  home\r\n/ # ");
        term
    }

    #[test]
    fn the_screen_paints_text_and_a_cursor_in_both_themes() {
        for (dark, palette, tag) in [(true, &DARK, "dark"), (false, &LIGHT, "light")] {
            let image = shoot(shell(), dark, 96, &format!("term-shell-{tag}.png"));
            // The background fills the window.
            assert_eq!(pixel(&image, 5, 5), rgb(palette.bg));
            assert_eq!(pixel(&image, 630, 390), rgb(palette.bg));
            // Text on row 0 (columns 0-10), none on the untouched row 10.
            let row = |n: i32| (PAD, PAD + n * LINE_H, PAD + 100, PAD + (n + 1) * LINE_H - 3);
            assert!(has_ink(&image, palette.bg, row(0)), "{tag}: text");
            assert!(!has_ink(&image, palette.bg, row(10)), "{tag}: blank row");
            // The cursor sits after "/ # " on row 4, in the cursor colour.
            let x = PAD + (4.0 * CELL_W) as i32 + 2;
            assert_eq!(pixel(&image, x, PAD + 4 * LINE_H + LINE_H - 2), rgb(palette.cursor));
        }
    }

    #[test]
    fn a_hidden_cursor_is_not_painted() {
        let mut term = shell();
        term.feed(b"\x1b[?25l");
        let image = shoot(term, true, 96, "term-cursor-hidden.png");
        let x = PAD + (4.0 * CELL_W) as i32 + 2;
        assert_eq!(pixel(&image, x, PAD + 4 * LINE_H + LINE_H - 2), rgb(DARK.bg));
    }

    #[test]
    fn the_metrics_double_at_twice_the_scale() {
        let image = shoot(shell(), true, 192, "term-shell-2x.png");
        let x = 2 * PAD + (2.0 * 4.0 * CELL_W) as i32 + 4;
        assert_eq!(
            pixel(&image, x, 2 * PAD + 2 * 4 * LINE_H + 2 * LINE_H - 2),
            rgb(DARK.cursor)
        );
    }

    #[test]
    fn a_full_screen_program_paints_from_the_alternate_screen() {
        let mut term = shell();
        term.feed(b"\x1b[?1049h\x1b[H\x1b[2J\x1b[1;1Hpager\x1b[2;1Hline two\x1b[?25l");
        let image = shoot(term, true, 96, "term-alternate.png");
        assert!(has_ink(&image, DARK.bg, (PAD, PAD, PAD + 60, PAD + LINE_H - 3)));
        // The shell's row 4 prompt is gone from view.
        assert!(!has_ink(&image, DARK.bg, (PAD, PAD + 4 * LINE_H, PAD + 100, PAD + 5 * LINE_H)));
    }

    #[test]
    fn wide_characters_and_attributes_paint_without_misplacing_the_row() {
        let mut term = Term::new(80, 24);
        term.feed("\x1b[1;31mred bold\x1b[0m \u{4e2d}\u{6587} plain\r\n".as_bytes());
        let image = shoot(term, false, 96, "term-wide-attrs.png");
        assert!(has_ink(&image, LIGHT.bg, (PAD, PAD, PAD + 200, PAD + LINE_H - 3)));
    }

    fn long_output() -> Term {
        let mut term = Term::new(80, 24);
        for line in 1..=300 {
            term.feed(format!("line {line} of the output\r\n").as_bytes());
        }
        term
    }

    #[test]
    fn a_scrolled_back_view_shows_old_lines_a_scrollbar_and_no_cursor() {
        let at_bottom = shoot_at(long_output(), 0, true, 96, "term-scroll-bottom.png");
        let back = shoot_at(long_output(), 120, true, 96, "term-scroll-back.png");
        // No scrollbar at the bottom, a pill in the gutter when scrolled back.
        let gutter = (640 - PAD, PAD, 640, 400 - PAD);
        assert!(!has_ink(&at_bottom, DARK.bg, gutter), "hidden at offset 0");
        assert!(has_ink(&back, DARK.bg, gutter), "shown while scrolled back");
        // The text differs and the cursor is not in view.
        let text = (PAD, PAD, 300, 400 - PAD);
        assert!(has_ink(&back, DARK.bg, text));
        let cursor_probe = |image: &Image| {
            (0..400).any(|y| (PAD..PAD + 9).any(|x| pixel(image, x, y) == rgb(DARK.cursor)))
        };
        assert!(cursor_probe(&at_bottom), "the cursor at the bottom");
        assert!(!cursor_probe(&back), "and not scrolled back");
    }

    /// The corner of the cell at (`col`, `row`), where no glyph reaches.
    fn cell_corner(col: usize, row: usize) -> (i32, i32) {
        (PAD + (col as f32 * CELL_W) as i32 + 1, PAD + row as i32 * LINE_H + LINE_H - 2)
    }

    #[test]
    fn a_selection_is_a_readable_band_behind_the_text() {
        for (dark, palette, tag) in [(true, &DARK, "dark"), (false, &LIGHT, "light")] {
            let mut term = Term::new(80, 24);
            term.feed(b"select these words please\r\nsecond row here");
            term.select_begin(lazyterm::SelectMode::Char, lazyterm::Point::new(0, 7));
            term.select_update(lazyterm::Point::new(1, 5));
            let image = shoot(term, dark, 96, &format!("term-selection-{tag}.png"));
            let at = |col, row| {
                let (x, y) = cell_corner(col, row);
                pixel(&image, x, y)
            };
            assert_eq!(at(7, 0), rgb(palette.select), "{tag}: first selected cell");
            assert_eq!(at(30, 0), rgb(palette.select), "{tag}: the rest of the first row");
            assert_eq!(at(5, 1), rgb(palette.select), "{tag}: last selected cell");
            assert_eq!(at(6, 1), rgb(palette.bg), "{tag}: after it");
            assert_eq!(at(6, 0), rgb(palette.bg), "{tag}: before it");
            // The band is far enough from the text colour to read it.
            let contrast = palette.fg.contrast_ratio(palette.select);
            assert!(contrast >= 4.5, "{tag}: text on the selection has contrast {contrast}");
        }
    }

    #[test]
    fn a_block_selection_paints_only_its_columns() {
        let mut term = Term::new(80, 24);
        term.feed(b"abcdefghij\r\nklmnopqrst\r\nuvwxyz");
        term.select_begin(lazyterm::SelectMode::Block, lazyterm::Point::new(0, 2));
        term.select_update(lazyterm::Point::new(2, 4));
        let image = shoot(term, true, 96, "term-selection-block.png");
        for row in 0..3 {
            let (x, y) = cell_corner(3, row);
            assert_eq!(pixel(&image, x, y), rgb(DARK.select), "row {row} column 3");
            let (x, y) = cell_corner(8, row);
            assert_eq!(pixel(&image, x, y), rgb(DARK.bg), "row {row} column 8");
        }
    }

    #[test]
    fn the_paste_confirmation_bar_has_two_buttons() {
        for (dark, palette, tag) in [(true, &DARK, "dark"), (false, &LIGHT, "light")] {
            let mut term = Term::new(80, 24);
            term.feed(b"prompt");
            let image = shoot_full(term, 0, Some(3), dark, 96, &format!("term-confirm-{tag}.png"));
            let layout = Layout { width: 640, height: 400, pad: PAD, line_h: LINE_H, visible: 19 };
            let bar = confirm_rects(&layout);
            let button = palette.bg.lerp(palette.fg, 0.32);
            assert_eq!(pixel(&image, bar.paste.left + 1, bar.paste.top + 1), rgb(button), "{tag}: Paste");
            assert_eq!(pixel(&image, bar.cancel.left + 1, bar.cancel.top + 1), rgb(button), "{tag}: Cancel");
            assert_eq!(pixel(&image, 3, bar.bar.top), rgb(palette.cursor), "{tag}: the bar's top edge");
            assert_ne!(pixel(&image, 3, bar.bar.top + 5), rgb(palette.bg), "{tag}: the bar has its own fill");
            assert_eq!(pixel(&image, 3, bar.bar.top - 2), rgb(palette.bg), "{tag}: above the bar");
        }
    }
}
