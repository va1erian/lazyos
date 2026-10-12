#![forbid(unsafe_code)]

//! The Text tool's editing state: a caret in a block of text placed on the
//! image, with no pixels until it is committed.

use crate::model::rect::Rect;
use crate::model::tools::Coverage;

use super::style::{TextCoverage, TextRasterizer, TextStyle};

/// The longest text a block accepts, in characters. Typing past it is ignored
/// so a stuck key cannot grow the block without bound.
pub const MAX_CHARS: usize = 100_000;

/// A text block being edited.
#[derive(Clone, Debug)]
pub struct TextEdit {
    chars: Vec<char>,
    caret: usize,
    /// The block's top-left corner in image coordinates.
    origin: (i32, i32),
    /// How the block is rasterized.
    pub style: TextStyle,
}

impl TextEdit {
    /// An empty block at `origin` (where the user clicked).
    pub fn new(origin: (i32, i32), style: TextStyle) -> TextEdit {
        TextEdit {
            chars: Vec::new(),
            caret: 0,
            origin,
            style,
        }
    }

    /// The text typed so far.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Whether nothing was typed.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The caret, as the number of characters before it.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The block's top-left corner.
    pub fn origin(&self) -> (i32, i32) {
        self.origin
    }

    /// Inserts a character at the caret; `\n` starts a new line and other
    /// control characters are ignored.
    pub fn insert(&mut self, c: char) {
        if (c.is_control() && c != '\n') || self.chars.len() >= MAX_CHARS {
            return;
        }
        self.chars.insert(self.caret, c);
        self.caret += 1;
    }

    /// Inserts a string (pasted text) at the caret, `\r\n` counting as one
    /// newline.
    pub fn insert_str(&mut self, text: &str) {
        let mut previous = '\0';
        for c in text.chars() {
            if !(previous == '\r' && c == '\n') {
                self.insert(if c == '\r' { '\n' } else { c });
            }
            previous = c;
        }
    }

    /// Starts a new line at the caret (Enter).
    pub fn newline(&mut self) {
        self.insert('\n');
    }

    /// Deletes the character before the caret.
    pub fn backspace(&mut self) {
        if self.caret > 0 {
            self.caret -= 1;
            self.chars.remove(self.caret);
        }
    }

    /// Deletes the character after the caret.
    pub fn delete(&mut self) {
        if self.caret < self.chars.len() {
            self.chars.remove(self.caret);
        }
    }

    /// Moves the caret one character left.
    pub fn left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    /// Moves the caret one character right.
    pub fn right(&mut self) {
        self.caret = (self.caret + 1).min(self.chars.len());
    }

    /// Moves the caret to the start of its line.
    pub fn home(&mut self) {
        while self.caret > 0 && self.chars[self.caret - 1] != '\n' {
            self.caret -= 1;
        }
    }

    /// Moves the caret to the end of its line.
    pub fn end(&mut self) {
        while self.caret < self.chars.len() && self.chars[self.caret] != '\n' {
            self.caret += 1;
        }
    }

    /// Moves the caret up one line, to the nearest x.
    pub fn up(&mut self, rasterizer: &dyn TextRasterizer) {
        self.step_line(rasterizer, -1);
    }

    /// Moves the caret down one line, to the nearest x.
    pub fn down(&mut self, rasterizer: &dyn TextRasterizer) {
        self.step_line(rasterizer, 1);
    }

    fn step_line(&mut self, rasterizer: &dyn TextRasterizer, by: isize) {
        let layout = self.geometry(rasterizer);
        let (line, _) = layout.line_column(self.caret);
        let Some(target) = line
            .checked_add_signed(by)
            .filter(|&t| t < layout.lines.len())
        else {
            return;
        };
        let Some((x, _, _)) = layout.caret(self.caret) else {
            return;
        };
        let to = &layout.lines[target];
        self.caret = layout.index_at(x, to.top + to.height / 2.0);
    }

    /// Puts the caret at the character nearest the image point `(x, y)`.
    pub fn set_caret_at(&mut self, rasterizer: &dyn TextRasterizer, x: i32, y: i32) {
        let layout = self.geometry(rasterizer);
        let index = layout.index_at(
            (i64::from(x) - i64::from(self.origin.0)) as f32,
            (i64::from(y) - i64::from(self.origin.1)) as f32,
        );
        self.caret = index.min(self.chars.len());
    }

    /// Moves the whole block by `(dx, dy)` pixels.
    pub fn move_by(&mut self, dx: i32, dy: i32) {
        self.origin = (
            self.origin.0.saturating_add(dx),
            self.origin.1.saturating_add(dy),
        );
    }

    /// Lays the text out with the current style, rasterizing all of it. The
    /// Text tool uses [`TextEdit::layout_in`] and [`TextEdit::geometry`].
    pub fn layout(&self, rasterizer: &dyn TextRasterizer) -> TextCoverage {
        rasterizer.layout(&self.text(), &self.style)
    }

    /// Lays the text out and rasterizes only `window` (the block's own
    /// coordinates), so the mask never outgrows the layer.
    pub fn layout_in(&self, rasterizer: &dyn TextRasterizer, window: Rect) -> TextCoverage {
        rasterizer.layout_in(&self.text(), &self.style, window)
    }

    /// The line and caret geometry alone: no pixels are rasterized.
    pub fn geometry(&self, rasterizer: &dyn TextRasterizer) -> TextCoverage {
        self.layout_in(rasterizer, Rect::EMPTY)
    }

    /// The caret as `(x, y, height)` in image coordinates, to draw as an
    /// overlay.
    pub fn caret_rect(&self, rasterizer: &dyn TextRasterizer) -> Option<(f32, f32, f32)> {
        let layout = self.geometry(rasterizer);
        let (x, y, height) = layout.caret(self.caret)?;
        Some((x + self.origin.0 as f32, y + self.origin.1 as f32, height))
    }

    /// The coverage to composite when the block is committed, placed at the
    /// block's origin in image coordinates. `None` for an empty block or one
    /// with no ink (only spaces).
    pub fn commit(&self, rasterizer: &dyn TextRasterizer) -> Option<Coverage> {
        if self.chars.is_empty() {
            return None;
        }
        let mask = self.layout(rasterizer).mask;
        (mask.total() > 0).then(|| mask.translated(self.origin.0, self.origin.1))
    }

    /// Abandons the block; consuming it makes "cancel leaves no trace" a
    /// property of the types rather than of the caller.
    pub fn cancel(self) {}
}
