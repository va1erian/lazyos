#![forbid(unsafe_code)]

//! The tool actions: begin/extend/end, fill and pick. A child of `model` so it
//! can touch the document's private fields. Every tool draws on the active
//! layer and records a delta: the rectangle it overwrote.

use super::{Drag, Model, Pixel, PreviewKind, Side, Tool};
use crate::model::history::UndoCapture;
use crate::model::{Edit, MAX_BRUSH, Preview, fill, raster};

/// What the eraser writes: no colour, no alpha.
const TRANSPARENT: Pixel = [0, 0, 0, 0];

impl Model {
    /// Selects a tool, dropping any in-progress preview.
    pub fn set_tool(&mut self, tool: Tool) {
        self.tool = tool;
        self.preview = None;
        self.drag = None;
    }

    /// Sets the brush diameter, clamped to `1..=MAX_BRUSH`.
    pub fn set_size(&mut self, size: u32) {
        self.size = size.clamp(1, MAX_BRUSH);
    }

    /// Sets the primary colour.
    pub fn set_primary(&mut self, color: Pixel) {
        self.primary = color;
    }

    /// Sets the secondary colour.
    pub fn set_secondary(&mut self, color: Pixel) {
        self.secondary = color;
    }

    /// Exchanges the primary and secondary colours.
    pub fn swap_colors(&mut self) {
        std::mem::swap(&mut self.primary, &mut self.secondary);
    }

    /// The paint colour for a side. The eraser's primary button writes
    /// transparent (alpha 0, so the layer shows the checkerboard where it
    /// erased); its secondary button paints the secondary colour.
    pub fn color_for(&self, side: Side) -> Pixel {
        match (self.tool, side) {
            (Tool::Eraser, Side::Primary) => TRANSPARENT,
            (_, Side::Primary) => self.primary,
            (_, Side::Secondary) => self.secondary,
        }
    }

    /// Begins an action at document pixel `(x, y)`.
    pub fn begin(&mut self, x: i32, y: i32, side: Side) {
        match self.tool {
            Tool::Pencil | Tool::Brush | Tool::Eraser => {
                let color = self.color_for(side);
                let diameter = if self.tool == Tool::Pencil {
                    1
                } else {
                    self.size
                };
                let layer = self.doc.active();
                let mut capture = UndoCapture::new();
                self.draw_dab(layer, &mut capture, (x, y), (x, y), diameter, color);
                self.drag = Some(Drag::Freehand {
                    layer,
                    last: (x, y),
                    diameter,
                    color,
                    capture,
                });
                self.bump();
            }
            Tool::Line | Tool::Rectangle | Tool::Ellipse => {
                self.drag = Some(Drag::Shape {
                    start: (x, y),
                    side,
                });
                self.preview = Some(self.preview_of((x, y), (x, y), side));
            }
            Tool::Fill => self.fill_at(x, y, side),
            Tool::Picker => self.pick(x, y, side),
        }
    }

    /// Extends an in-progress action to `(x, y)`.
    pub fn extend(&mut self, x: i32, y: i32) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        self.drag = Some(match drag {
            Drag::Freehand {
                layer,
                last,
                diameter,
                color,
                mut capture,
            } => {
                let last = if last != (x, y) {
                    self.draw_dab(layer, &mut capture, last, (x, y), diameter, color);
                    self.bump();
                    (x, y)
                } else {
                    last
                };
                Drag::Freehand {
                    layer,
                    last,
                    diameter,
                    color,
                    capture,
                }
            }
            Drag::Shape { start, side } => {
                self.preview = Some(self.preview_of(start, (x, y), side));
                Drag::Shape { start, side }
            }
        });
    }

    /// Finishes an action, committing it to the history as one step.
    pub fn end(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Freehand { layer, capture, .. } => self.commit(layer, capture),
            Drag::Shape { start, side } => {
                let end = self.preview.map(|p| p.to).unwrap_or(start);
                let layer = self.doc.active();
                let color = self.color_for(side);
                let diameter = self.size;
                let mut capture = UndoCapture::new();
                if let Some(target) = self.doc.layer_mut(layer) {
                    capture.touch(&target.pixels, raster::reach(start, end, diameter));
                    let bitmap = &mut target.pixels;
                    match self.tool {
                        Tool::Line => raster::line(bitmap, start, end, diameter, color),
                        Tool::Rectangle => raster::rectangle(bitmap, start, end, diameter, color),
                        Tool::Ellipse => raster::ellipse(bitmap, start, end, diameter, color),
                        _ => {}
                    }
                }
                self.commit(layer, capture);
                self.bump();
            }
        }
        self.preview = None;
    }

    /// Abandons an in-progress action without leaving the tool stuck: a freehand
    /// stroke is committed, a shape is committed at its last preview point.
    pub fn cancel(&mut self) {
        self.end();
    }

    /// Whether a drag is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Samples the pixel at `(x, y)` of the active layer into the given side.
    pub fn pick(&mut self, x: i32, y: i32, side: Side) {
        let Some(pixel) = self.bitmap().get(x, y) else {
            return;
        };
        match side {
            Side::Primary => self.primary = pixel,
            Side::Secondary => self.secondary = pixel,
        }
    }

    /// Stamps the segment `from`..`to` on `layer`, backing up what it covers
    /// first and marking it for repaint.
    fn draw_dab(
        &mut self,
        layer: usize,
        capture: &mut UndoCapture,
        from: (i32, i32),
        to: (i32, i32),
        diameter: u32,
        color: Pixel,
    ) {
        let Some(target) = self.doc.layer_mut(layer) else {
            return;
        };
        let area = raster::reach(from, to, diameter).intersect(target.pixels.rect());
        if area.is_empty() {
            return;
        }
        capture.touch(&target.pixels, area);
        raster::line(&mut target.pixels, from, to, diameter, color);
        self.damage_rect(area);
    }

    /// Turns a finished capture into one history entry named for the tool.
    fn commit(&mut self, layer: usize, capture: UndoCapture) {
        let Some(target) = self.doc.layer(layer) else {
            return;
        };
        let touched = capture.bounds();
        if let Some((rect, before)) = capture.finish(&target.pixels) {
            self.damage_rect(rect);
            self.history.record(Edit::Pixels {
                name: self.tool.label().to_string(),
                layer,
                rect,
                before,
            });
        } else if !touched.is_empty() {
            self.damage_rect(touched);
        }
    }

    /// The bucket: fills the clicked region of the active layer.
    fn fill_at(&mut self, x: i32, y: i32, side: Side) {
        let color = self.color_for(side);
        let layer = self.doc.active();
        let Some(target) = self.doc.layer_mut(layer) else {
            return;
        };
        let Some(filled) = fill::flood_fill_spans(&mut target.pixels, x, y, color) else {
            return;
        };
        // Every filled pixel was `target`: rebuild the picture before the fill.
        let Some(mut before) = target.pixels.crop(filled.bounds) else {
            return;
        };
        for (row, x0, x1) in &filled.spans {
            before.fill_span(
                row - filled.bounds.y0,
                x0 - filled.bounds.x0,
                x1 - filled.bounds.x0,
                filled.target,
            );
        }
        self.damage_rect(filled.bounds);
        self.history.record(Edit::Pixels {
            name: self.tool.label().to_string(),
            layer,
            rect: filled.bounds,
            before,
        });
        self.bump();
    }

    fn preview_of(&self, from: (i32, i32), to: (i32, i32), side: Side) -> Preview {
        let kind = match self.tool {
            Tool::Line => PreviewKind::Line,
            Tool::Rectangle => PreviewKind::Rectangle,
            _ => PreviewKind::Ellipse,
        };
        Preview {
            kind,
            from,
            to,
            color: self.color_for(side),
            diameter: self.size,
        }
    }

    pub(super) fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}
