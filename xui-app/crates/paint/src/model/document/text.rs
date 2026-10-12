#![forbid(unsafe_code)]

//! The Text tool in the model: a block being edited and the pixels it shows.
//!
//! The text is painted into the layer on every change (see [`Live`]) in the
//! primary colour, so what is on screen is what commit keeps. Commit is one
//! history entry; Esc takes the pixels back. The caret and the block's box are
//! kept as numbers for the view to draw, because they are never pixels.

use std::rc::Rc;

use super::live::{ClipMask, Live, clip_of};
use super::{Drag, Model};
use crate::model::text::{TextCoverage, TextEdit, TextRasterizer, TextStyle};
use crate::model::tools::{PaintMode, StrokeBuffer};
use crate::model::{Pixel, Rect};

/// The name of a committed block in the history.
const NAME: &str = "Text";

/// A key the Text tool edits with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextKey {
    /// Caret one character left.
    Left,
    /// Caret one character right.
    Right,
    /// Caret one line up.
    Up,
    /// Caret one line down.
    Down,
    /// Caret to the start of the line.
    Home,
    /// Caret to the end of the line.
    End,
    /// Delete the character before the caret.
    Backspace,
    /// Delete the character after the caret.
    Delete,
    /// Start a new line.
    Enter,
    /// Keep the text as pixels (Ctrl+Enter).
    Commit,
    /// Drop the text (Esc).
    Cancel,
}

/// What the live paint last drew, to skip a repaint that would change nothing.
type Drawn = (String, TextStyle, (i32, i32), Pixel);

/// A text block being edited.
pub(super) struct TextSession {
    pub(super) edit: TextEdit,
    live: Live,
    drawn: Option<Drawn>,
    clip: Option<ClipMask>,
}

/// Where the caret and the block are, in picture pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct TextGeom {
    /// `(x, y, height)` of the caret.
    caret: (f32, f32, f32),
    /// The ink and the line boxes.
    block: Rect,
}

impl Model {
    /// Gives the Text tool its rasterizer; without one the tool does nothing.
    pub fn set_text_rasterizer(&mut self, rasterizer: Rc<dyn TextRasterizer>) {
        self.rasterizer = Some(rasterizer);
    }

    /// The rasterizer, if the host supplied one.
    pub fn text_rasterizer(&self) -> Option<&Rc<dyn TextRasterizer>> {
        self.rasterizer.as_ref()
    }

    /// Whether a text block is being edited.
    pub fn is_editing_text(&self) -> bool {
        self.text.is_some()
    }

    /// The caret as `(x, y, height)` in picture pixels.
    pub fn text_caret(&self) -> Option<(f32, f32, f32)> {
        self.text_geom.map(|geom| geom.caret)
    }

    /// The box around the text being edited, in picture pixels.
    pub fn text_block(&self) -> Option<Rect> {
        self.text_geom.map(|geom| geom.block)
    }

    /// The handle that drags the block: a square at its top-left corner,
    /// `handle_px` picture pixels on a side (the view picks that size so the
    /// handle looks the same at every zoom).
    pub fn text_handle(&self) -> Option<Rect> {
        let block = self.text_block()?;
        let side = self.text_handle_px.max(1);
        Some(Rect::new(
            block.x0.saturating_sub(side as i32),
            block.y0.saturating_sub(side as i32),
            side,
            side,
        ))
    }

    /// Sets the handle's side in picture pixels.
    pub fn set_text_handle_px(&mut self, side: u32) {
        self.text_handle_px = side.clamp(1, 256);
    }

    /// A click with the Text tool: on the handle it starts moving the block,
    /// in the block it puts the caret there, elsewhere it keeps the block and
    /// starts a new one at the click.
    pub(super) fn begin_text(&mut self, x: i32, y: i32) {
        let Some(rasterizer) = self.rasterizer.clone() else {
            self.notice = Some("No fonts are installed for the Text tool".to_string());
            return;
        };
        if let Some(from) = self.text.as_ref().map(|s| s.edit.origin()) {
            if self.text_handle().is_some_and(|h| h.contains(x, y)) {
                self.drag = Some(Drag::MoveText {
                    start: (x, y),
                    from,
                });
                return;
            }
            if self.text_block().is_some_and(|block| block.contains(x, y)) {
                if let Some(session) = &mut self.text {
                    session.edit.set_caret_at(&*rasterizer, x, y);
                }
                self.refresh_text();
                return;
            }
        }
        self.commit_text();
        self.text = Some(TextSession {
            edit: TextEdit::new((x, y), self.text_style.clone()),
            live: Live::new(self.doc.active()),
            drawn: None,
            clip: ClipMask::of(self.doc.selection()),
        });
        self.refresh_text();
    }

    /// Drags the block by its handle.
    pub(super) fn drag_text(&mut self, start: (i32, i32), from: (i32, i32), x: i32, y: i32) {
        if let Some(session) = &mut self.text {
            let (ox, oy) = session.edit.origin();
            let to = (
                from.0.saturating_add(x.saturating_sub(start.0)),
                from.1.saturating_add(y.saturating_sub(start.1)),
            );
            session.edit.move_by(to.0 - ox, to.1 - oy);
        }
        self.refresh_text();
    }

    /// Types a character at the caret; control characters are ignored (Enter
    /// is [`TextKey::Enter`]).
    pub fn type_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        if let Some(session) = &mut self.text {
            session.edit.insert(c);
            self.refresh_text();
        }
    }

    /// Inserts pasted text at the caret.
    pub fn type_str(&mut self, text: &str) {
        if let Some(session) = &mut self.text {
            session.edit.insert_str(text);
            self.refresh_text();
        }
    }

    /// An editing key.
    pub fn text_key(&mut self, key: TextKey) {
        match key {
            TextKey::Commit => return self.commit_text(),
            TextKey::Cancel => return self.cancel_text(),
            _ => {}
        }
        let Some(rasterizer) = self.rasterizer.clone() else {
            return;
        };
        let Some(session) = &mut self.text else {
            return;
        };
        let edit = &mut session.edit;
        match key {
            TextKey::Left => edit.left(),
            TextKey::Right => edit.right(),
            TextKey::Up => edit.up(&*rasterizer),
            TextKey::Down => edit.down(&*rasterizer),
            TextKey::Home => edit.home(),
            TextKey::End => edit.end(),
            TextKey::Backspace => edit.backspace(),
            TextKey::Delete => edit.delete(),
            TextKey::Enter => edit.newline(),
            TextKey::Commit | TextKey::Cancel => {}
        }
        self.refresh_text();
    }

    /// Lays the block out, updates the caret and box, and repaints the
    /// pixels when the text, the style, the place or the colour changed.
    pub(super) fn refresh_text(&mut self) {
        let Some(mut session) = self.text.take() else {
            return;
        };
        let Some(rasterizer) = self.rasterizer.clone() else {
            self.text = Some(session);
            return;
        };
        let layout = session.edit.layout(&*rasterizer);
        let (ox, oy) = session.edit.origin();
        self.text_geom = geometry(&layout, session.edit.caret(), (ox, oy));
        let key = (
            session.edit.text(),
            session.edit.style.clone(),
            (ox, oy),
            self.primary,
        );
        if session.drawn.as_ref() != Some(&key) {
            self.paint_text(&mut session, &layout);
            session.drawn = Some(key);
        }
        self.text = Some(session);
    }

    /// Redraws the glyphs through a stroke buffer, taking the last frame back
    /// first.
    fn paint_text(&mut self, session: &mut TextSession, layout: &TextCoverage) {
        let (ox, oy) = session.edit.origin();
        let mask = layout.mask.clone().translated(ox, oy);
        let inked = mask.total() > 0;
        let color = self.primary;
        let Some(target) = self.doc.layer_mut(session.live.layer) else {
            return;
        };
        let reach = if inked { mask.rect() } else { Rect::EMPTY };
        let clip = clip_of(&session.clip);
        let changed = session.live.repaint(&mut target.pixels, reach, |bitmap| {
            if !inked {
                return Rect::EMPTY;
            }
            let mut buffer = StrokeBuffer::new(bitmap);
            buffer.add_coverage(bitmap, &mask);
            buffer.composite(bitmap, color, PaintMode::Paint, clip)
        });
        self.damage_rect(changed);
        self.bump();
    }

    /// Keeps the text as pixels: one history entry.
    pub fn commit_text(&mut self) {
        let Some(session) = self.text.take() else {
            return;
        };
        self.text_geom = None;
        let layer = session.live.layer;
        self.commit_named(layer, session.live.into_capture(), NAME);
    }

    /// Drops the text and its pixels; nothing is recorded.
    pub fn cancel_text(&mut self) {
        let Some(session) = self.text.take() else {
            return;
        };
        self.text_geom = None;
        if let Some(target) = self.doc.layer_mut(session.live.layer) {
            let changed = session.live.cancel(&mut target.pixels);
            self.damage_rect(changed);
            self.bump();
        }
    }
}

/// The caret and the box of a laid-out block placed at `origin`.
fn geometry(layout: &TextCoverage, caret: usize, origin: (i32, i32)) -> Option<TextGeom> {
    let (cx, cy, height) = layout.caret(caret)?;
    let last = layout.lines.last()?;
    let ink = layout.mask.rect();
    let local = Rect {
        x0: 0,
        y0: 0,
        x1: ink.x1.max(2),
        y1: ((last.top + last.height).ceil() as i32).max(ink.y1).max(1),
    };
    let block = Rect {
        x0: local.x0.saturating_add(origin.0),
        y0: local.y0.saturating_add(origin.1),
        x1: local.x1.saturating_add(origin.0),
        y1: local.y1.saturating_add(origin.1),
    };
    Some(TextGeom {
        caret: (cx + origin.0 as f32, cy + origin.1 as f32, height),
        block,
    })
}
