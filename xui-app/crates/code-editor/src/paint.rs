#![forbid(unsafe_code)]

//! The editor's painter: a monospace grid.
//!
//! One character cell's advance and one line's height are measured once per
//! paint; every other position is `col * advance`. Only the visible lines are
//! drawn, so a 5,000-line file costs the same as a short one. xui has no
//! caret-from-offset query, so the grid does its own arithmetic.

use xui_core::Color;
use xui_core::backend::{Canvas, TextAlign, TextVAlign};
use xui_core::geometry::{Point, Rect};
use xui_core::theme::Theme;

use crate::lexer::{Token, TokenClass};
use crate::markers::MarkerKind;
use crate::metrics::{CELL_PROBE, Metrics, Viewport};
use crate::state::EditorState;
use crate::text::{display_col, expand_tabs};
use crate::theme::EditorTheme;
use crate::view::caret_display_col;
use xui_core::widget::scrollbar::{self, Orientation, Scroll};

/// Draws `state` into `canvas`.
///
/// xui's software compositor reports [`Canvas::bounds`] as the node's window
/// rectangle with no translation applied, so the painter moves the origin to
/// the node's corner and clips to it, then works in node-local coordinates, as
/// the mouse events do. On a canvas that honours the contract the bounds already
/// sit at the origin and this is a no-op.
pub(crate) fn paint(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    xui_theme: &Theme,
) {
    let origin = canvas.bounds();
    let area = Rect::from_size(origin.size());
    canvas.save();
    canvas.set_translation(origin.left as f32, origin.top as f32);
    canvas.push_clip(area);
    paint_local(canvas, state, theme, xui_theme, area);
    canvas.pop_clip();
    canvas.restore();
}

/// Draws `state` into `bounds`, the node's area at the canvas origin.
fn paint_local(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    xui_theme: &Theme,
    bounds: Rect,
) {
    let dpi = canvas.dpi();
    let style = state.options.font.style(theme.text);
    let measured = canvas.measure_text(CELL_PROBE, &style);
    let line_count = state.buffer.line_count();
    let metrics = Metrics::new(measured, line_count, state.options.show_gutter, dpi);
    let viewport = Viewport::split(
        bounds,
        metrics,
        line_count,
        state.buffer.max_line_cols(state.options.tab_width),
        dpi,
    );
    let first_line = state.view.first_line.min(line_count.saturating_sub(1));
    let last_line = (first_line + viewport.visible_lines).min(line_count);
    let first_col = state.view.first_col;

    canvas.fill_rect(bounds, theme.background);
    if state.options.show_gutter {
        canvas.fill_rect(viewport.gutter, theme.gutter_background);
    }

    paint_current_line(canvas, state, theme, &viewport, first_line, line_count);
    paint_marker_tints(canvas, state, theme, &viewport, first_line, last_line);

    if let Some((start, end)) = state.view.selection() {
        let color = if state.focused {
            theme.selection
        } else {
            theme.selection_unfocused
        };
        paint_selection(
            canvas, state, &viewport, first_line, last_line, start, end, color,
        );
    }

    paint_brackets(canvas, state, theme, &viewport, first_line, last_line);
    paint_lines(canvas, state, theme, &viewport, first_line, last_line);
    paint_squiggles(canvas, state, theme, &viewport, first_line, last_line);
    paint_gutter(canvas, state, theme, &viewport, first_line, last_line);
    paint_caret(canvas, state, theme, &viewport, first_line, last_line);

    paint_scrollbars(canvas, state, &viewport, first_line, first_col, xui_theme);

    let border = if state.focused {
        theme.border_focused
    } else {
        theme.border
    };
    canvas.stroke_rect(bounds, border, 1.0);
    if state.selected {
        canvas.stroke_rect(bounds, xui_theme.accent, 2.0);
    }
}

/// The current line's highlight, behind the text.
fn paint_current_line(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    line_count: usize,
) {
    if !state.focused {
        return;
    }
    let line = state.buffer.line_of_char(state.view.caret);
    if line < first_line || line >= first_line + viewport.visible_lines || line >= line_count {
        return;
    }
    let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
    let row = Rect::new(
        viewport.text.left,
        y,
        viewport.text.right,
        y + viewport.metrics.line_height,
    );
    canvas.fill_rect(row, theme.current_line);
}

/// Faint tints behind lines carrying error or warning markers.
fn paint_marker_tints(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    for marker in &state.markers {
        if marker.line < first_line || marker.line >= last_line {
            continue;
        }
        let color = match marker.kind {
            MarkerKind::Error => theme.error,
            MarkerKind::Warning => theme.warning,
            _ => continue,
        };
        let y = viewport
            .metrics
            .y_of_line(viewport.text, marker.line, first_line);
        let row = Rect::new(
            viewport.gutter.left,
            y,
            viewport.text.right,
            y + viewport.metrics.line_height,
        );
        canvas.fill_rect(row, color.lerp(theme.background, 0.85));
    }
}

/// The selection fill, clipped to the visible text area.
#[allow(clippy::too_many_arguments)]
fn paint_selection(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
    start: usize,
    end: usize,
    color: Color,
) {
    let text = viewport.text;
    let metrics = viewport.metrics;
    let tab = state.options.tab_width;
    let first_col = state.view.first_col;
    let start_line = state.buffer.line_of_char(start);
    let end_line = state.buffer.line_of_char(end);
    canvas.push_clip(text);
    for line in start_line..=end_line {
        if line < first_line || line >= last_line {
            continue;
        }
        let line_start = state.buffer.line_start(line);
        let line_text = state.buffer.line_string(line);
        let line_len = line_text.chars().count();
        let from = if line == start_line {
            start.saturating_sub(line_start)
        } else {
            0
        };
        let to = if line == end_line {
            end.saturating_sub(line_start)
        } else {
            line_len
        };
        let x0 = metrics.x_of_col(text, display_col(&line_text, from, tab), first_col);
        let mut end_col = display_col(&line_text, to, tab);
        if to == line_len && line < end_line {
            // The newline cell is selected too.
            end_col += 1;
        }
        let mut x1 = metrics.x_of_col(text, end_col, first_col);
        if x1 <= x0 {
            x1 = x0 + metrics.advance;
        }
        let y = metrics.y_of_line(text, line, first_line);
        let cell = Rect::new(x0, y, x1, y + metrics.line_height);
        if let Some(cell) = intersect(cell, text) {
            canvas.fill_rect(cell, color);
        }
    }
    canvas.pop_clip();
}

/// The visible lines' text, coloured by lexical class.
///
/// Each token is mapped from char offsets to display columns through the raw
/// line (tabs expand to several cells), then drawn one character per cell.
/// Tokens entirely off-screen are skipped, and only a token's visible columns
/// (plus one either side for overhang) are drawn; the text clip trims the rest.
fn paint_lines(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    let text = viewport.text;
    let metrics = viewport.metrics;
    let tab = state.options.tab_width;
    let first_col = state.view.first_col;
    let last_col = first_col + viewport.visible_cols;
    canvas.push_clip(text);
    for line in first_line..last_line {
        let raw = state.buffer.line_string(line);
        let line_chars: Vec<char> = expand_tabs(&raw, tab).chars().collect();
        let y = metrics.y_of_line(text, line, first_line);
        // A plain-text cache stores no tokens, so treat a tokenless line as one
        // whole plain token: the default highlighter still shows the text (in
        // the same colour as an identifier, which is the editor's text token).
        let cached = state.highlight.tokens(line);
        let plain;
        let tokens = if cached.is_empty() && !raw.is_empty() {
            plain = [Token {
                start: 0,
                end: raw.chars().count(),
                class: TokenClass::Identifier,
            }];
            plain.as_slice()
        } else {
            cached
        };
        for token in tokens {
            let start = display_col(&raw, token.start, tab);
            let end = display_col(&raw, token.end, tab);
            // Visibility uses the same one-column overhang margin as the drawn
            // range, so a token ending just left of the view still draws the
            // glyph that reaches into it.
            let visible = drawn_cols(start..end, first_col, last_col);
            if visible.is_empty() {
                continue;
            }
            let style = state.options.font.style(theme.token_color(token.class));
            // Each character is drawn in its own cell. The advance is a whole
            // number of pixels while the font's is fractional, so a whole run
            // drawn at once drifts from the grid: its tail is clipped and the
            // next token covers it. The rect spans two cells so a glyph a
            // little wider than its rounded cell is not clipped either.
            // Only the visible columns are drawn, plus one either side for a
            // glyph that overhangs its cell, so a long token off to the side
            // costs nothing on each repaint.
            let drawn = whole_clusters(&line_chars, visible, start..end);
            let chars = &line_chars[drawn.clone()];
            for (offset, cluster) in clusters(chars) {
                if cluster == " " {
                    continue;
                }
                let x = metrics.x_of_col(text, drawn.start + offset, first_col);
                let cell = Rect::new(x, y, x + metrics.advance * 2, y + metrics.line_height);
                canvas.draw_text(&cluster, cell, &style);
            }
        }
    }
    canvas.pop_clip();
}

/// The columns of a token spanning `token` that are worth drawing when
/// `first_col..last_col` is visible: the visible part, plus one column either
/// side for a glyph that overhangs its cell. Empty when nothing is visible.
fn drawn_cols(
    token: std::ops::Range<usize>,
    first_col: usize,
    last_col: usize,
) -> std::ops::Range<usize> {
    let from = token.start.max(first_col.saturating_sub(1));
    let to = token.end.min(last_col + 1);
    from..to.max(from)
}

/// Widens `cols` so it neither starts nor ends inside a cluster: the start moves
/// back to its cluster's base and the end forward over trailing marks, both
/// within `token` and the line. A view scrolled so that a base character sits
/// just off the left edge still draws its accent attached, not on its own.
fn whole_clusters(
    line: &[char],
    cols: std::ops::Range<usize>,
    token: std::ops::Range<usize>,
) -> std::ops::Range<usize> {
    let floor = token.start;
    let ceiling = token.end.min(line.len());
    let continues =
        |index: usize| index > 0 && (attaches(line[index]) || line[index - 1] == '\u{200D}');
    let mut start = cols.start.min(ceiling);
    while start > floor && continues(start) {
        start -= 1;
    }
    let mut end = cols.end.min(ceiling).max(start);
    while end < ceiling && continues(end) {
        end += 1;
    }
    start..end
}

/// Splits `chars` into the runs drawn together, each with its offset: a base
/// character plus the combining marks, variation selectors and zero-width-joined
/// characters that follow it, so an accent or a joined emoji is shaped with its
/// base instead of on its own. The grid is still one column per `char`, so the
/// caller advances by the offset, not by the cluster.
fn clusters(chars: &[char]) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut joined = false;
    for (index, &character) in chars.iter().enumerate() {
        match out.last_mut() {
            Some((_, cluster)) if joined || attaches(character) => cluster.push(character),
            _ => out.push((index, character.to_string())),
        }
        joined = character == '\u{200D}';
    }
    out
}

/// Whether `character` attaches to the one before it rather than starting a
/// cell of its own: a combining mark, a variation selector or a zero-width
/// joiner. A small table stands in for full grapheme segmentation, which this
/// crate avoids a dependency for.
fn attaches(character: char) -> bool {
    matches!(
        character,
        '\u{0300}'..='\u{036F}'
            | '\u{1AB0}'..='\u{1AFF}'
            | '\u{1DC0}'..='\u{1DFF}'
            | '\u{20D0}'..='\u{20FF}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FE20}'..='\u{FE2F}'
            | '\u{200D}'
            | '\u{E0100}'..='\u{E01EF}'
    )
}

/// A fill behind the bracket pair at the caret, if any.
fn paint_brackets(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.focused {
        return;
    }
    let Some((open, close)) = state
        .highlight
        .bracket_pair(&state.buffer, state.view.caret)
    else {
        return;
    };
    for position in [open, close] {
        let line = state.buffer.line_of_char(position);
        if line < first_line || line >= last_line {
            continue;
        }
        let start = state.buffer.line_start(line);
        let text = state.buffer.line_string(line);
        let col = display_col(&text, position - start, state.options.tab_width);
        let x = viewport
            .metrics
            .x_of_col(viewport.text, col, state.view.first_col);
        let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
        let cell = Rect::new(
            x,
            y,
            x + viewport.metrics.advance,
            y + viewport.metrics.line_height,
        );
        if let Some(cell) = intersect(cell, viewport.text) {
            canvas.fill_rect(cell, theme.bracket_match);
        }
    }
}

/// A squiggle under each spanned marker.
fn paint_squiggles(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    let text = viewport.text;
    let metrics = viewport.metrics;
    let tab = state.options.tab_width;
    let first_col = state.view.first_col;
    canvas.push_clip(text);
    for marker in &state.markers {
        if !marker.has_span() || marker.line < first_line || marker.line >= last_line {
            continue;
        }
        let color = match marker.kind {
            MarkerKind::Error => theme.error,
            MarkerKind::Warning => theme.warning,
            _ => theme.gutter_text,
        };
        let line_text = state.buffer.line_string(marker.line);
        let start = display_col(&line_text, marker.start, tab);
        let end = display_col(&line_text, marker.end, tab).max(start + 1);
        let x0 = metrics.x_of_col(text, start, first_col);
        let x1 = metrics.x_of_col(text, end, first_col);
        let y = metrics.y_of_line(text, marker.line, first_line) + metrics.line_height - 2;
        if let Some(span) = intersect(Rect::new(x0, y, x1, y + 2), text) {
            canvas.draw_line(
                Point::new(span.left, span.top),
                Point::new(span.right, span.top),
                color,
                1.0,
            );
        }
    }
    canvas.pop_clip();
}

/// Line numbers and breakpoint dots.
fn paint_gutter(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.options.show_gutter {
        return;
    }
    let gutter = viewport.gutter;
    let metrics = viewport.metrics;
    let pad = 4;
    let mut number_style = state.options.font.style(theme.gutter_text);
    number_style.align = TextAlign::End;
    number_style.valign = TextVAlign::Middle;
    for line in first_line..last_line {
        let y = metrics.y_of_line(viewport.text, line, first_line);
        let row = Rect::new(
            gutter.left + pad,
            y,
            gutter.right - pad,
            y + metrics.line_height,
        );
        canvas.draw_text(&(line + 1).to_string(), row, &number_style);
    }
    for marker in &state.markers {
        if marker.kind != MarkerKind::Breakpoint
            || marker.line < first_line
            || marker.line >= last_line
        {
            continue;
        }
        let center = Point::new(
            gutter.left + pad,
            metrics.y_of_line(viewport.text, marker.line, first_line) + metrics.line_height / 2,
        );
        canvas.fill_ellipse(center, 3.0, 3.0, theme.breakpoint);
    }
}

/// The blinking caret.
fn paint_caret(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    theme: &EditorTheme,
    viewport: &Viewport,
    first_line: usize,
    last_line: usize,
) {
    if !state.focused || !state.blink_on {
        return;
    }
    let line = state.buffer.line_of_char(state.view.caret);
    if line < first_line || line >= last_line {
        return;
    }
    let col = caret_display_col(&state.buffer, state.options.tab_width, state.view.caret);
    let x = viewport
        .metrics
        .x_of_col(viewport.text, col, state.view.first_col);
    if x < viewport.text.left || x >= viewport.text.right {
        return;
    }
    let y = viewport.metrics.y_of_line(viewport.text, line, first_line);
    canvas.draw_line(
        Point::new(x, y),
        Point::new(x, y + viewport.metrics.line_height),
        theme.caret,
        1.0,
    );
}

/// The vertical and horizontal scrollbars.
fn paint_scrollbars(
    canvas: &mut dyn Canvas,
    state: &EditorState,
    viewport: &Viewport,
    first_line: usize,
    first_col: usize,
    xui_theme: &Theme,
) {
    let metrics = viewport.metrics;
    let line_count = state.buffer.line_count();
    if let Some(track) = viewport.vbar {
        let scroll = Scroll {
            viewport: viewport.text.height(),
            content: line_count as i32 * metrics.line_height,
            offset: first_line as i32 * metrics.line_height,
        };
        scrollbar::paint_state(
            canvas,
            track,
            scroll,
            Orientation::Vertical,
            *xui_theme,
            scrollbar::ThumbState::Normal,
        );
    }
    if let Some(track) = viewport.hbar {
        let scroll = Scroll {
            viewport: viewport.text.width(),
            content: state.buffer.max_line_cols(state.options.tab_width) as i32 * metrics.advance,
            offset: first_col as i32 * metrics.advance,
        };
        scrollbar::paint_state(
            canvas,
            track,
            scroll,
            Orientation::Horizontal,
            *xui_theme,
            scrollbar::ThumbState::Normal,
        );
    }
}

/// The intersection of two rectangles, or `None` when they do not overlap.
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right.min(b.right);
    let bottom = a.bottom.min(b.bottom);
    (left < right && top < bottom).then(|| Rect::new(left, top, right, bottom))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::Marker;
    use crate::options::Options;
    use crate::platform::InProcessClipboard;
    use crate::state::EditorState;
    use crate::theme::EditorTheme;
    use xui_canvas::{RgbaImage, Surface};

    #[test]
    fn only_the_visible_columns_of_a_long_token_are_drawn() {
        // A 10 000-column token with columns 100..180 on screen.
        assert_eq!(drawn_cols(0..10_000, 100, 180), 99..181);
        // A token entirely inside the view is drawn whole.
        assert_eq!(drawn_cols(120..130, 100, 180), 120..130);
        // At the left edge nothing underflows.
        assert_eq!(drawn_cols(0..5, 0, 80), 0..5);
        // A token past the view draws nothing.
        assert!(drawn_cols(500..600, 100, 180).is_empty());
        // A token ending at the view's left edge keeps its last column, whose
        // glyph can overhang into view; one ending further left draws nothing.
        assert_eq!(drawn_cols(90..100, 100, 180), 99..100);
        assert!(drawn_cols(90..99, 100, 180).is_empty());
    }

    #[test]
    fn combining_marks_and_joined_characters_are_drawn_with_their_base() {
        let chars = |text: &str| text.chars().collect::<Vec<_>>();
        // "e" + combining acute, then "x": the accent stays with the "e", and
        // the "x" keeps its own column (2, since the accent takes one).
        assert_eq!(
            clusters(&chars("e\u{0301}x")),
            [(0, "e\u{0301}".to_owned()), (2, "x".to_owned())]
        );
        // A ZWJ sequence is one run.
        let family = "\u{1F468}\u{200D}\u{1F469}";
        assert_eq!(clusters(&chars(family)), [(0, family.to_owned())]);
        // Plain ASCII is one cell per character.
        assert_eq!(
            clusters(&chars("ab")),
            [(0, "a".to_owned()), (1, "b".to_owned())]
        );
    }

    #[test]
    fn a_cluster_cut_by_the_left_edge_is_drawn_whole() {
        // "xe" + two combining marks + "y": columns 0..5.
        let line: Vec<char> = "xe\u{0301}\u{0323}y".chars().collect();
        // Scrolled so the drawn range would start on the second mark (col 3):
        // it moves back to the "e" at col 1, not past the token start.
        assert_eq!(whole_clusters(&line, 3..5, 0..5), 1..5);
        assert_eq!(
            whole_clusters(&line, 3..5, 2..5),
            2..5,
            "bounded by the token"
        );
        // A range ending inside a cluster takes the trailing marks too.
        assert_eq!(whole_clusters(&line, 0..2, 0..5), 0..4);
        // Plain text is unchanged.
        let plain: Vec<char> = "abcdef".chars().collect();
        assert_eq!(whole_clusters(&plain, 2..4, 0..6), 2..4);
    }

    fn render(state: &EditorState) -> RgbaImage {
        let xui_theme = Theme::light();
        let editor_theme = EditorTheme::from_theme(xui_theme);
        let mut surface = Surface::new(240, 80);
        surface.with_canvas_at(Rect::new(0, 0, 240, 80), 96, |canvas| {
            paint(canvas, state, &editor_theme, &xui_theme);
        });
        surface.to_image()
    }

    fn editor_state(text: &str) -> EditorState {
        let mut state = EditorState::new(text, Options::default(), Box::new(InProcessClipboard));
        state.focused = true;
        state
    }

    /// An editor state that highlights Rhai, for the token-colour tests.
    #[cfg(feature = "rhai-syntax")]
    fn rhai_editor_state(text: &str) -> EditorState {
        let mut state = EditorState::with_highlighter(
            text,
            Options::default(),
            Box::new(InProcessClipboard),
            Box::new(crate::lexer::RhaiHighlighter),
        );
        state.focused = true;
        state
    }

    fn count_color(image: &RgbaImage, color: Color) -> usize {
        let mut count = 0;
        for y in 0..image.height {
            for x in 0..image.width {
                if image.pixel(x, y) == Some([color.r, color.g, color.b, 0xFF]) {
                    count += 1;
                }
            }
        }
        count
    }

    /// How close the nearest painted pixel gets to `color`, in RGB distance.
    #[cfg(feature = "rhai-syntax")]
    fn nearest_distance(image: &RgbaImage, color: Color) -> f32 {
        let mut best = f32::MAX;
        for y in 0..image.height {
            for x in 0..image.width {
                let Some([r, g, b, _]) = image.pixel(x, y) else {
                    continue;
                };
                let distance = (f32::from(r) - f32::from(color.r)).powi(2)
                    + (f32::from(g) - f32::from(color.g)).powi(2)
                    + (f32::from(b) - f32::from(color.b)).powi(2);
                best = best.min(distance.sqrt());
            }
        }
        best
    }

    #[test]
    fn text_and_selection_are_painted() {
        let mut state = editor_state("hello\nworld");
        state.view.anchor = 0;
        state.view.caret = 5;
        let theme = EditorTheme::from_theme(Theme::light());
        let image = render(&state);

        assert!(
            count_color(&image, theme.selection) > 0,
            "the selection fill is drawn"
        );
        let blank = render(&editor_state(""));
        assert_ne!(
            image.pixels, blank.pixels,
            "a selected document differs from a blank one"
        );
    }

    #[test]
    fn plain_text_is_painted() {
        // The default `PlainText` highlighter emits no tokens; the painter must
        // still draw the line. Unfocused and with the caret off, an empty buffer
        // paints only the background and border, so any difference is glyphs.
        let mut with_text = editor_state("hello");
        with_text.focused = false;
        with_text.blink_on = false;
        let mut blank = editor_state("");
        blank.focused = false;
        blank.blink_on = false;
        assert_ne!(
            render(&with_text).pixels,
            render(&blank).pixels,
            "a plain-text line is still painted"
        );
    }

    #[test]
    fn the_caret_is_painted_only_when_visible() {
        let mut state = editor_state("abc");
        state.view.caret = 1;
        state.view.anchor = 1;
        state.blink_on = true;
        let shown = render(&state);
        state.blink_on = false;
        let hidden = render(&state);
        assert_ne!(shown.pixels, hidden.pixels, "the blink hides the caret");
    }

    #[cfg(feature = "rhai-syntax")]
    #[test]
    fn syntax_classes_are_painted_in_their_theme_colours() {
        let state = rhai_editor_state("let total = 42;");
        let theme = EditorTheme::from_theme(Theme::light());
        let image = render(&state);
        let keyword = nearest_distance(&image, theme.keyword);
        let number = nearest_distance(&image, theme.number);
        assert!(
            keyword < 60.0,
            "the keyword is painted in the keyword colour (distance {keyword})"
        );
        assert!(
            number < 60.0,
            "the number is painted in the number colour (distance {number})"
        );
    }

    #[test]
    fn markers_paint_a_squiggle() {
        let mut state = editor_state("let x = ;");
        let plain = render(&state);
        state.markers = vec![Marker::new(0, 8, 8, MarkerKind::Error)];
        let marked = render(&state);
        assert_ne!(
            plain.pixels, marked.pixels,
            "the squiggle changes the rendered pixels"
        );
    }
}
