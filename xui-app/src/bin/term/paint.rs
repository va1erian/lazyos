//! The Terminal's painter: the engine's screen, scrollback and selection as
//! pixels. Per row: backgrounds first, then the selection, then text (one
//! string per run when the font's advance is a whole pixel count, else one
//! glyph per cell; box-drawing and block characters as geometry), then
//! underline and strike, and finally the cursor. Every glyph is placed from
//! its column, never from the width of a string, so a wide or fallback
//! glyph cannot move what follows it.

use lazyterm::boxdraw::{geometry, is_geometric, Box2, Fill};
use lazyterm::render::{runs, Run};
use lazyterm::settings::CursorShape;
use lazyterm::{Attr, Flags, Palette, Rgb, Term};
use xui_core::{Canvas, Color, Rect};

use super::badge;
use super::geometry::confirm_rects;
use super::look::Look;
use super::view::{
    first_row, intersect, line_number, max_offset, overlaps, thumb, view_line, Layout,
};


fn color(rgb: Rgb) -> Color {
    Color::rgb(rgb.0, rgb.1, rgb.2)
}

/// Everything one frame is drawn from.
pub struct Scene<'a> {
    pub term: &'a Term,
    pub look: &'a Look,
    /// Lines the view is scrolled back from the bottom.
    pub offset: usize,
    /// The paste confirmation bar's line count, while it is up.
    pub pending: Option<usize>,
    /// The theme's accent: the selection is the background tinted with it.
    pub accent: Rgb,
    /// The size badge's text, while it is up (`badge.rs`).
    pub badge: Option<&'a str>,
}

/// The background of selected cells: the scheme's background tinted with the
/// theme's accent, far enough from the text colour to read it.
pub fn selection_color(palette: &Palette, accent: Rgb) -> Rgb {
    let mut tint = 0.40;
    let mut color = palette.bg().lerp(accent, tint);
    while color.contrast(palette.fg()) < 4.5 && tint > 0.05 {
        tint -= 0.05;
        color = palette.bg().lerp(accent, tint);
    }
    color
}

/// A cell's colours: foreground and background after bold-is-bright, inverse,
/// dim and hidden.
pub fn cell_colors(palette: &Palette, attr: Attr, bold_is_bright: bool) -> (Rgb, Rgb) {
    let bold = attr.flags.has(Flags::BOLD);
    let mut fg = palette.resolve(attr.fg, true, bold, bold_is_bright);
    let mut bg = palette.resolve(attr.bg, false, false, false);
    if attr.flags.has(Flags::INVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    if attr.flags.has(Flags::DIM) {
        fg = fg.lerp(bg, 0.5);
    }
    if attr.flags.has(Flags::HIDDEN) {
        fg = bg;
    }
    (fg, bg)
}

/// The pixel box of columns `col..col + cells` of the row at `top`.
struct Cells {
    left: i32,
    top: i32,
    cell_w: i32,
    line_h: i32,
}

impl Cells {
    fn rect(&self, col: usize, cells: usize) -> Rect {
        let x0 = self.left + col as i32 * self.cell_w;
        Rect::new(x0, self.top, x0 + cells as i32 * self.cell_w, self.top + self.line_h)
    }
}

/// Paint the view into `canvas`, only inside `damage` (node pixels; `None`
/// paints everything). Returns the layout the next drain turns changed rows
/// into rectangles with, and the pointer into cells.
pub fn paint(canvas: &mut dyn Canvas, scene: &Scene, damage: Option<Rect>) -> Layout {
    let (term, look) = (scene.term, scene.look);
    let m = &look.metrics;
    let palette = term.palette();
    let bounds = canvas.bounds();
    let damage = intersect(damage.unwrap_or(bounds), bounds);
    canvas.fill_rect(damage, color(palette.bg()));
    let rows = term.rows();
    let visible_cols = ((bounds.width() - 2 * m.pad) / m.cell_w).max(0) as usize;
    let visible = (((bounds.height() - 2 * m.pad) / m.line_h).max(1) as usize).min(rows);
    let cols = visible_cols.min(term.cols());
    let (cursor_row, cursor_col) = term.cursor();
    let first = first_row(cursor_row, visible, rows);
    let select = selection_color(palette, scene.accent);
    for slot in 0..visible {
        let top = bounds.top + m.pad + slot as i32 * m.line_h;
        let row = Rect::new(bounds.left, top, bounds.right, top + m.line_h);
        if !overlaps(row, damage) {
            continue;
        }
        let Some(line) = view_line(term, first, scene.offset, slot) else {
            continue;
        };
        let cells = Cells {
            left: bounds.left + m.pad,
            top,
            cell_w: m.cell_w,
            line_h: m.line_h,
        };
        let found = runs(line.cells, cols);
        let span = line_number(term, first, scene.offset, slot)
            .and_then(|line| term.selection_span(line));
        draw_row(canvas, scene, &cells, &found, span.filter(|&(from, _)| from < cols), select, cols);
    }
    // The cursor is a screen row: it is in view only while its slot is.
    let slot = (cursor_row + scene.offset).saturating_sub(first);
    if term.cursor_visible() && cursor_row >= first && slot < visible && cursor_col < cols {
        let top = bounds.top + m.pad + slot as i32 * m.line_h;
        let cells = Cells {
            left: bounds.left + m.pad,
            top,
            cell_w: m.cell_w,
            line_h: m.line_h,
        };
        draw_cursor(canvas, scene, &cells, view_line(term, first, scene.offset, slot), cursor_col, damage);
    }
    if let Some((top, bottom)) = thumb(
        bounds.top + m.pad,
        bounds.height() - 2 * m.pad,
        max_offset(term, visible),
        scene.offset,
        visible,
        16 * m.scale,
    ) {
        // A thin pill in the right gutter, between the background and the text.
        let (left, right) = (bounds.right - m.pad * 7 / 10, bounds.right - m.pad * 3 / 10);
        let rect = Rect::new(left, top, right, bottom);
        if overlaps(rect, damage) {
            let pill = palette.bg().lerp(palette.fg(), 0.45);
            canvas.fill_rounded_rect(rect, (right - left) as f32 / 2.0, color(pill));
        }
    }
    let laid = Layout {
        width: bounds.width(),
        height: bounds.height(),
        pad: m.pad,
        line_h: m.line_h,
        cell_w: m.cell_w,
        visible,
    };
    if let Some(lines) = scene.pending {
        paint_confirm(canvas, palette, &laid, m.scale, lines, damage);
    }
    if let Some(text) = scene.badge {
        badge::paint(canvas, palette, look, &laid, text, damage);
    }
    laid
}

fn draw_row(
    canvas: &mut dyn Canvas,
    scene: &Scene,
    cells: &Cells,
    found: &[Run],
    selected: Option<(usize, usize)>,
    select: Rgb,
    cols: usize,
) {
    let palette = scene.term.palette();
    let bib = scene.look.bold_is_bright;
    // Backgrounds, then the selection over them.
    for run in found {
        let (_, bg) = cell_colors(palette, run.attr, bib);
        if bg != palette.bg() {
            canvas.fill_rect(cells.rect(run.col, run.cells.min(cols - run.col)), color(bg));
        }
    }
    if let Some((from, to)) = selected {
        let to = to.min(cols - 1);
        let rect = Rect::new(cells.rect(from, 1).left, cells.top, cells.rect(to, 1).right, cells.top + cells.line_h);
        canvas.fill_rect(rect, color(select));
    }
    // Text.
    for run in found {
        let (fg, bg) = cell_colors(palette, run.attr, bib);
        if !run.is_blank() && fg != bg {
            draw_run(canvas, scene.look, cells, run, fg, bg, cols);
        }
    }
    // Underline and strike, over the glyphs.
    let light = scene.look.metrics.light;
    for run in found {
        let (fg, _) = cell_colors(palette, run.attr, bib);
        let span = cells.rect(run.col, run.cells.min(cols - run.col));
        if run.attr.flags.has(Flags::UNDERLINE) {
            let y = cells.top + cells.line_h - 2 * light;
            canvas.fill_rect(Rect::new(span.left, y, span.right, y + light), color(fg));
        }
        if run.attr.flags.has(Flags::STRIKE) {
            let y = cells.top + (cells.line_h - light) / 2;
            canvas.fill_rect(Rect::new(span.left, y, span.right, y + light), color(fg));
        }
    }
}

/// Whether a glyph is plain printable ASCII: drawn with its neighbours when
/// the font's advance is exact.
fn simple(c: char) -> bool {
    ('!'..='~').contains(&c)
}

fn draw_run(
    canvas: &mut dyn Canvas,
    look: &Look,
    cells: &Cells,
    run: &Run,
    fg: Rgb,
    bg: Rgb,
    cols: usize,
) {
    let m = &look.metrics;
    let bold = run.attr.flags.has(Flags::BOLD);
    let italic = run.attr.flags.has(Flags::ITALIC);
    let style = look.text_style(color(fg), bold, italic);
    // A bold the family has no face for is the regular face overstruck.
    let overstrike = bold && !look.styles.bold;
    let mut text = |canvas: &mut dyn Canvas, s: &str, col: usize, width: usize| {
        let at = cells.rect(col, width);
        // Wide enough that nothing is clipped; the glyphs are placed from `at.left`.
        let reach = (s.chars().count() as i32 + 1) * 2 * m.cell_w;
        let rect = Rect::new(at.left, at.top + m.text_dy, at.left + reach, at.bottom);
        canvas.draw_text(s, rect, &style);
        if overstrike {
            canvas.draw_text(s, Rect::new(rect.left + m.scale, rect.top, rect.right, rect.bottom), &style);
        }
    };
    let mut segment = String::new();
    let mut segment_col = 0;
    let flush = |canvas: &mut dyn Canvas,
                 segment: &mut String,
                 segment_col: usize,
                 text: &mut dyn FnMut(&mut dyn Canvas, &str, usize, usize)| {
        let trimmed = segment.trim_end_matches(' ');
        if !trimmed.is_empty() {
            if m.exact {
                text(canvas, trimmed, segment_col, trimmed.chars().count());
            } else {
                // One glyph per cell: the font's advance is not a whole
                // pixel count, so a string would drift off the grid.
                for (i, ch) in trimmed.chars().enumerate() {
                    if ch != ' ' {
                        text(canvas, &ch.to_string(), segment_col + i, 1);
                    }
                }
            }
        }
        segment.clear();
    };
    for (offset, ch, width) in run.glyphs() {
        let col = run.col + offset;
        if col >= cols {
            break;
        }
        if simple(ch) || (ch == ' ' && !segment.is_empty()) {
            if segment.is_empty() {
                segment_col = col;
            }
            segment.push(ch);
            continue;
        }
        flush(canvas, &mut segment, segment_col, &mut text);
        if ch == ' ' {
            continue;
        }
        let cell = cells.rect(col, width);
        if is_geometric(ch) {
            draw_geometry(canvas, ch, cell, m.light, fg, bg);
        } else {
            text(canvas, &ch.to_string(), col, width);
        }
    }
    flush(canvas, &mut segment, segment_col, &mut text);
}

fn draw_geometry(canvas: &mut dyn Canvas, ch: char, cell: Rect, light: i32, fg: Rgb, bg: Rgb) {
    let at = Box2::new(cell.left, cell.top, cell.right, cell.bottom);
    for (b, fill) in geometry(ch, at, light).unwrap_or_default() {
        let paint = match fill {
            Fill::Solid => fg,
            Fill::Quarters(q) => bg.lerp(fg, f32::from(q) / 4.0),
        };
        canvas.fill_rect(Rect::new(b.x0, b.y0, b.x1, b.y1), color(paint));
    }
}

/// The cursor: a block (with its character redrawn in the background colour),
/// an underline or a bar while the window has the focus and the blink phase is
/// on; a hollow block outline while it does not.
fn draw_cursor(
    canvas: &mut dyn Canvas,
    scene: &Scene,
    cells: &Cells,
    line: Option<lazyterm::LineRef>,
    col: usize,
    damage: Rect,
) {
    let (term, look) = (scene.term, scene.look);
    let m = &look.metrics;
    let palette = term.palette();
    let under = line.and_then(|l| l.cells.get(col).copied()).unwrap_or_default();
    let width = usize::from(under.width.max(1));
    let rect = cells.rect(col, width);
    if !overlaps(rect, damage) {
        return;
    }
    let cursor = color(palette.cursor());
    if !look.focused {
        let t = m.light;
        for edge in [
            Rect::new(rect.left, rect.top, rect.right, rect.top + t),
            Rect::new(rect.left, rect.bottom - t, rect.right, rect.bottom),
            Rect::new(rect.left, rect.top, rect.left + t, rect.bottom),
            Rect::new(rect.right - t, rect.top, rect.right, rect.bottom),
        ] {
            canvas.fill_rect(edge, cursor);
        }
        return;
    }
    if !look.cursor_on {
        return;
    }
    match look.cursor_shape {
        CursorShape::Block => {
            canvas.fill_rect(rect, cursor);
            if under.ch != ' ' && under.width != 0 {
                let (_, bg) = cell_colors(palette, under.attr, look.bold_is_bright);
                let glyph = Run {
                    col,
                    cells: width,
                    text: under.ch.to_string(),
                    attr: Attr::default(),
                };
                draw_run(canvas, look, cells, &glyph, bg, palette.cursor(), usize::MAX);
            }
        }
        CursorShape::Underline => {
            let h = 3 * m.scale;
            canvas.fill_rect(Rect::new(rect.left, rect.bottom - h, rect.right, rect.bottom), cursor);
        }
        CursorShape::Bar => {
            let w = 2 * m.scale;
            canvas.fill_rect(Rect::new(rect.left, rect.top, rect.left + w, rect.bottom), cursor);
        }
    }
}

/// The paste confirmation bar: the question, a Paste and a Cancel button
/// (Enter and Escape do the same).
fn paint_confirm(
    canvas: &mut dyn Canvas,
    palette: &Palette,
    layout: &Layout,
    scale: i32,
    lines: usize,
    damage: Rect,
) {
    let c = confirm_rects(layout);
    if !overlaps(c.bar, damage) {
        return;
    }
    let fg = color(palette.fg());
    let style = xui_core::TextStyle::new(fg, xui_core::Dip(15.0));
    canvas.fill_rect(c.bar, color(palette.bg().lerp(palette.fg(), 0.16)));
    canvas.fill_rect(Rect::new(c.bar.left, c.bar.top, c.bar.right, c.bar.top + scale), color(palette.cursor()));
    let text = |canvas: &mut dyn Canvas, text: &str, rect: Rect| {
        let at = Rect::new(rect.left, rect.top + (rect.height() - 20 * scale) / 2, rect.right, rect.bottom);
        canvas.draw_text(text, at, &style);
    };
    let question = format!("Paste {lines} lines?");
    text(canvas, &question, Rect::new(c.bar.left + layout.pad, c.bar.top, c.paste.left, c.bar.bottom));
    for (rect, label) in [(c.paste, "Paste"), (c.cancel, "Cancel")] {
        canvas.fill_rect(rect, color(palette.bg().lerp(palette.fg(), 0.32)));
        text(canvas, label, Rect::new(rect.left + layout.pad, rect.top, rect.right, rect.bottom));
    }
}

#[cfg(test)]
#[path = "paint_tests.rs"]
mod tests;
