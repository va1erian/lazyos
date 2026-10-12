#![forbid(unsafe_code)]

//! The tool actions: begin/extend/end, fill and pick. A child of `model` so it
//! can touch the document's private fields. Every tool draws on the active
//! layer and records a delta: the rectangle it overwrote.

use super::alpha;
use super::{Drag, Model, Pixel, PreviewKind, Side, Tool};
use crate::model::history::UndoCapture;
use crate::model::selection::mix_by;
use crate::model::{Bitmap, Edit, MAX_BRUSH, Mods, Preview, fill, raster};

/// What the eraser writes: no colour, no alpha.
const TRANSPARENT: Pixel = [0, 0, 0, 0];

impl Model {
    /// Selects a tool, dropping any in-progress preview. Pixels being placed
    /// (a lift or a paste) land first, unless the new tool is Move Selected
    /// Pixels, which keeps placing them.
    pub fn set_tool(&mut self, tool: Tool) {
        self.end();
        if tool != Tool::MovePixels {
            self.commit_float();
        }
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

    /// Begins an action at document pixel `(x, y)` with no modifier held.
    pub fn begin(&mut self, x: i32, y: i32, side: Side) {
        self.begin_with(x, y, side, Mods::NONE);
    }

    /// Begins an action at document pixel `(x, y)`. The modifiers matter to
    /// the selection tools.
    pub fn begin_with(&mut self, x: i32, y: i32, side: Side, mods: Mods) {
        match self.tool {
            Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::MagicWand => {
                self.begin_select(x, y, side, mods);
            }
            Tool::MovePixels | Tool::MoveSelection => self.begin_move(x, y),
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
        self.extend_with(x, y, Mods::NONE);
    }

    /// Extends an in-progress action to `(x, y)` with `mods` held now.
    pub fn extend_with(&mut self, x: i32, y: i32, mods: Mods) {
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
            drag @ (Drag::Select { .. } | Drag::Lasso { .. }) => {
                self.extend_select(drag, x, y, mods)
            }
            drag @ (Drag::MoveOutline { .. } | Drag::MoveFloat { .. }) => {
                self.extend_move(drag, x, y)
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
                if let Some((target, selection)) = self.doc.layer_and_selection(layer) {
                    let reach = raster::reach(start, end, diameter);
                    capture.touch(&target.pixels, reach);
                    let bitmap = &mut target.pixels;
                    let tool = self.tool;
                    let draw = move |bitmap: &mut Bitmap, at: (i32, i32), color: Pixel| {
                        let (a, b) = (shift(start, at), shift(end, at));
                        match tool {
                            Tool::Line => raster::line(bitmap, a, b, diameter, color),
                            Tool::Rectangle => raster::rectangle(bitmap, a, b, diameter, color),
                            Tool::Ellipse => raster::ellipse(bitmap, a, b, diameter, color),
                            _ => {}
                        }
                    };
                    if alpha::blends(tool, color) {
                        alpha::paint_over(bitmap, &capture, reach, color, draw);
                    } else {
                        draw(bitmap, (0, 0), color);
                    }
                    capture.clip(bitmap, reach, selection);
                }
                self.commit(layer, capture);
                self.bump();
            }
            Drag::Select { base, .. } | Drag::Lasso { base, .. } => self.end_select(base),
            Drag::MoveOutline { base, .. } => self.end_move_outline(base),
            // The lifted pixels stay in the air until committed.
            Drag::MoveFloat { .. } => {}
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
        let Some((target, selection)) = self.doc.layer_and_selection(layer) else {
            return;
        };
        let area = raster::reach(from, to, diameter).intersect(target.pixels.rect());
        if area.is_empty() {
            return;
        }
        capture.touch(&target.pixels, area);
        let draw = |bitmap: &mut Bitmap, at: (i32, i32), color: Pixel| {
            raster::line(bitmap, shift(from, at), shift(to, at), diameter, color);
        };
        if alpha::blends(self.tool, color) {
            alpha::paint_over(&mut target.pixels, capture, area, color, draw);
        } else {
            draw(&mut target.pixels, (0, 0), color);
        }
        capture.clip(&mut target.pixels, area, selection);
        self.damage_rect(area);
    }

    /// Turns a finished capture into one history entry named for the tool.
    fn commit(&mut self, layer: usize, capture: UndoCapture) {
        let name = self.tool.label().to_string();
        self.commit_named(layer, capture, &name);
    }

    /// Turns a finished capture into one history entry called `name`.
    pub(super) fn commit_named(&mut self, layer: usize, capture: UndoCapture, name: &str) {
        let Some(target) = self.doc.layer(layer) else {
            return;
        };
        let touched = capture.bounds();
        if let Some((rect, before)) = capture.finish(&target.pixels) {
            self.damage_rect(rect);
            self.history.record(Edit::Pixels {
                name: name.to_string(),
                layer,
                rect,
                before,
            });
            self.bump();
        } else if !touched.is_empty() {
            self.damage_rect(touched);
        }
    }

    /// The bucket: fills the clicked region of the active layer.
    fn fill_at(&mut self, x: i32, y: i32, side: Side) {
        let color = self.color_for(side);
        let layer = self.doc.active();
        let Some((target, selection)) = self.doc.layer_and_selection(layer) else {
            return;
        };
        // A click outside the selection fills nothing.
        if selection.is_some_and(|s| s.coverage(x, y) == 0) {
            return;
        }
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
        // The region was found without regard to the selection; take back
        // what lies outside it.
        if let Some(selection) = selection {
            let rect = filled.bounds;
            for py in rect.y0..rect.y1 {
                for px in rect.x0..rect.x1 {
                    let cover = selection.coverage(px, py);
                    if cover == 255 {
                        continue;
                    }
                    if let (Some(old), Some(new)) = (
                        before.get(px - rect.x0, py - rect.y0),
                        target.pixels.get(px, py),
                    ) {
                        target.pixels.put(px, py, mix_by(cover, old, new));
                    }
                }
            }
            if target.pixels.crop(rect).as_ref() == Some(&before) {
                return;
            }
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

/// `point` moved by `by`, without overflowing.
fn shift(point: (i32, i32), by: (i32, i32)) -> (i32, i32) {
    (point.0.saturating_add(by.0), point.1.saturating_add(by.1))
}
