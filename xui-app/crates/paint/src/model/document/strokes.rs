#![forbid(unsafe_code)]

//! The freehand tools: Pencil, Paintbrush and Eraser.
//!
//! One [`StrokeBuffer`] per drag holds the stroke's coverage, so a stroke that
//! crosses itself does not build up alpha, composites it once per move and
//! hands the history its "before" picture. Esc takes the pixels back.

use super::live::{ClipMask, clip_of};
use super::{Drag, Model};
use crate::model::tools::{BrushOptions, PaintMode, StrokeBuffer, pixel_center};
use crate::model::{Mods, Pixel, Rect, Side, Tool};

/// A freehand stroke in progress.
pub(super) struct Stroke {
    layer: usize,
    buffer: StrokeBuffer,
    color: Pixel,
    mode: PaintMode,
    /// The brush; `None` is the Pencil's exact pixel line.
    brush: Option<BrushOptions>,
    last: (i32, i32),
    clip: Option<ClipMask>,
}

impl Model {
    /// How a freehand tool paints with the button on `side`.
    fn stroke_paint(&self, side: Side) -> (PaintMode, Pixel) {
        match (self.tool, side) {
            // The eraser's own button erases; the other paints the secondary.
            (Tool::Eraser, Side::Primary) => (PaintMode::Erase, [0, 0, 0, 255]),
            (Tool::Pencil, _) => (PaintMode::Replace, self.color_for(side)),
            _ => (PaintMode::Paint, self.color_for(side)),
        }
    }

    /// Starts a stroke at pixel `(x, y)`; Shift with the Pencil continues a
    /// line from where the last pencil stroke ended.
    pub(super) fn begin_stroke(&mut self, x: i32, y: i32, side: Side, mods: Mods) {
        let (mode, color) = self.stroke_paint(side);
        let layer = self.doc.active();
        let clip = ClipMask::of(self.doc.selection());
        let brush = (self.tool != Tool::Pencil).then(|| self.brush());
        let from = match brush {
            None if mods.shift => self.pencil_last.unwrap_or((x, y)),
            _ => (x, y),
        };
        let Some(target) = self.doc.layer_mut(layer) else {
            return;
        };
        let mut buffer = StrokeBuffer::new(&target.pixels);
        match brush {
            None => {
                buffer.pixel_line(&target.pixels, from, (x, y));
            }
            Some(brush) => {
                buffer.dab(&target.pixels, pixel_center(x, y), brush);
            }
        }
        let changed = buffer.composite(&mut target.pixels, color, mode, clip_of(&clip));
        self.damage_rect(changed);
        self.bump();
        self.drag = Some(Drag::Stroke(Stroke {
            layer,
            buffer,
            color,
            mode,
            brush,
            last: (x, y),
            clip,
        }));
    }

    /// Continues a stroke to `(x, y)`.
    pub(super) fn extend_stroke(&mut self, mut stroke: Stroke, x: i32, y: i32) -> Stroke {
        if stroke.last == (x, y) {
            return stroke;
        }
        if let Some(target) = self.doc.layer_mut(stroke.layer) {
            match stroke.brush {
                None => {
                    stroke
                        .buffer
                        .pixel_line(&target.pixels, stroke.last, (x, y));
                }
                Some(brush) => {
                    let from = pixel_center(stroke.last.0, stroke.last.1);
                    let to = pixel_center(x, y);
                    stroke.buffer.segment(&target.pixels, from, to, brush);
                }
            }
            let clip = clip_of(&stroke.clip);
            let changed = stroke
                .buffer
                .composite(&mut target.pixels, stroke.color, stroke.mode, clip);
            self.damage_rect(changed);
            self.bump();
        }
        stroke.last = (x, y);
        stroke
    }

    /// Finishes a stroke: one history entry from the buffer's "before".
    pub(super) fn end_stroke(&mut self, stroke: Stroke) {
        if stroke.brush.is_none() {
            self.pencil_last = Some(stroke.last);
        }
        self.record_buffer(stroke.layer, stroke.buffer, Rect::EMPTY);
    }

    /// Esc: puts the layer back as the stroke found it.
    pub(super) fn cancel_stroke(&mut self, stroke: Stroke) {
        if let Some(target) = self.doc.layer_mut(stroke.layer) {
            let changed = stroke.buffer.cancel(&mut target.pixels);
            self.damage_rect(changed);
            self.bump();
        }
    }
}
