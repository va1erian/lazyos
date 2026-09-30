//! Text painting: the visible lines' glyphs, coloured by lexical class, plus
//! the cluster/column arithmetic that maps char offsets to grid cells.

use xui_core::backend::Canvas;
use xui_core::geometry::Rect;

use crate::lexer::{Token, TokenClass};
use crate::metrics::Viewport;
use crate::state::EditorState;
use crate::text::{display_col, expand_tabs};
use crate::theme::EditorTheme;

/// The visible lines' text, coloured by lexical class.
///
/// Each token is mapped from char offsets to display columns through the raw
/// line (tabs expand to several cells), then drawn one character per cell.
/// Tokens entirely off-screen are skipped, and only a token's visible columns
/// (plus one either side for overhang) are drawn; the text clip trims the rest.
pub(super) fn paint_lines(
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
pub(super) fn drawn_cols(
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
pub(super) fn whole_clusters(
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
pub(super) fn clusters(chars: &[char]) -> Vec<(usize, String)> {
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
