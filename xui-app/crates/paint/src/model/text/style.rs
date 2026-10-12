#![forbid(unsafe_code)]

//! Text style, the rasterizer contract and the geometry an editing caret needs.

use crate::model::rect::Rect;
use crate::model::tools::Coverage;

/// Horizontal alignment of each line within the widest one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TextAlign {
    /// Lines start at the block's left edge.
    #[default]
    Left,
    /// Lines are centred on the block.
    Center,
    /// Lines end at the block's right edge.
    Right,
}

/// How text is rasterized: the Text tool's options bar binds to this.
#[derive(Clone, Debug, PartialEq)]
pub struct TextStyle {
    /// Font family name, as registered with the rasterizer.
    pub family: String,
    /// Font size in pixels.
    pub size: f32,
    /// Bold weight.
    pub bold: bool,
    /// Italic slant.
    pub italic: bool,
    /// An underline under every line.
    pub underline: bool,
    /// Alignment of the lines.
    pub align: TextAlign,
    /// Antialiasing; off gives hard 0/255 coverage.
    pub antialias: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            family: "Liberation Sans".to_owned(),
            size: 24.0,
            bold: false,
            italic: false,
            underline: false,
            align: TextAlign::Left,
            antialias: true,
        }
    }
}

/// Where one line of text sits, in the coverage's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct LineGeometry {
    /// The line box's top edge.
    pub top: f32,
    /// The line box's height.
    pub height: f32,
    /// The baseline's y.
    pub baseline: f32,
    /// The index (in characters) of the line's first character in the text.
    pub start: usize,
    /// The caret x before each character of the line and after the last one
    /// (so one more than the line has characters, newline not counted),
    /// alignment included.
    pub carets: Vec<f32>,
}

impl LineGeometry {
    /// How many characters the line holds, not counting its newline.
    pub fn len(&self) -> usize {
        self.carets.len().saturating_sub(1)
    }

    /// Whether the line has no character.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A laid-out and rasterized block of text.
#[derive(Clone, Debug, PartialEq)]
pub struct TextCoverage {
    /// The rectangle the whole block's ink and line boxes span, its origin at
    /// the block's top-left (the point the user clicked): the size of the
    /// block however little of it `mask` holds.
    pub extent: Rect,
    /// The glyph coverage inside the window asked for: `extent` clipped to
    /// it, in the same coordinates. A tall line of wide text is far bigger
    /// than any layer, so the part nobody can see is never allocated.
    pub mask: Coverage,
    /// One entry per line of the text, top first (an empty text has one line).
    pub lines: Vec<LineGeometry>,
}

impl TextCoverage {
    /// A layout from the complete `full` coverage, keeping only what lies in
    /// `window`: for rasterizers that draw the whole block first.
    pub fn windowed(full: Coverage, window: Rect, lines: Vec<LineGeometry>) -> TextCoverage {
        TextCoverage {
            extent: full.rect(),
            mask: full.cropped(window),
            lines,
        }
    }

    /// The line holding the caret before character `index` (the end of text is
    /// the last line's end; a caret right after a newline starts the next line).
    fn line_of(&self, index: usize) -> Option<(usize, &LineGeometry)> {
        let at = self
            .lines
            .iter()
            .rposition(|line| line.start <= index)
            .unwrap_or(0);
        self.lines.get(at).map(|line| (at, line))
    }

    /// The caret before character `index` as `(x, top, height)`.
    pub fn caret(&self, index: usize) -> Option<(f32, f32, f32)> {
        let (_, line) = self.line_of(index)?;
        let column = (index - line.start).min(line.len());
        Some((line.carets[column], line.top, line.height))
    }

    /// The caret index nearest to the point `(x, y)` in the same coordinates.
    pub fn index_at(&self, x: f32, y: f32) -> usize {
        let Some(line) = self
            .lines
            .iter()
            .rfind(|line| line.top <= y)
            .or(self.lines.first())
        else {
            return 0;
        };
        let column = line
            .carets
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))
            .map_or(0, |(i, _)| i);
        line.start + column
    }

    /// The line number and column of the caret before character `index`.
    pub fn line_column(&self, index: usize) -> (usize, usize) {
        match self.line_of(index) {
            Some((at, line)) => (at, (index - line.start).min(line.len())),
            None => (0, 0),
        }
    }
}

/// Turns text into coverage and caret geometry. The real implementation is
/// behind the crate's `text` feature; the model and its tests use fakes.
pub trait TextRasterizer {
    /// Lays `text` out (lines split at `\n`, no wrapping) and rasterizes the
    /// part inside `window` (the block's own coordinates, origin at its
    /// top-left) in white-on-nothing coverage. The caret geometry and the
    /// `extent` always describe the whole block; only `mask` is clipped, and
    /// an empty window rasterizes nothing.
    fn layout_in(&self, text: &str, style: &TextStyle, window: Rect) -> TextCoverage;

    /// [`TextRasterizer::layout_in`] with no window: the whole block's
    /// coverage, which grows with the text. For tests and callers that know
    /// the text is small; the Text tool always passes the layer's window.
    fn layout(&self, text: &str, style: &TextStyle) -> TextCoverage {
        self.layout_in(
            text,
            style,
            Rect {
                x0: 0,
                y0: 0,
                x1: i32::MAX,
                y1: i32::MAX,
            },
        )
    }

    /// The family names the options bar offers, sorted. A rasterizer that
    /// cannot list its fonts offers none.
    fn families(&self) -> Vec<String> {
        Vec::new()
    }
}
