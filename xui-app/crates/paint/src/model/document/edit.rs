#![forbid(unsafe_code)]

//! The model's actions: begin/extend/end, fill, pick, undo/redo, clear and
//! load. A child of `model` so it can touch the document's private fields.

use super::{Drag, Model, Pixel, PreviewKind, Side, Tool};
use crate::model::{Bitmap, MAX_BRUSH, Preview, WHITE, fill, raster};

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

    /// The paint colour for a side (the eraser always paints the background).
    pub fn color_for(&self, side: Side) -> Pixel {
        if self.tool == Tool::Eraser {
            return WHITE;
        }
        match side {
            Side::Primary => self.primary,
            Side::Secondary => self.secondary,
        }
    }

    /// Begins an action at canvas pixel `(x, y)`.
    pub fn begin(&mut self, x: i32, y: i32, side: Side) {
        match self.tool {
            Tool::Pencil | Tool::Brush | Tool::Eraser => {
                let color = self.color_for(side);
                let diameter = if self.tool == Tool::Pencil {
                    1
                } else {
                    self.size
                };
                let before = self.bitmap.clone();
                raster::stamp(&mut self.bitmap, x, y, diameter, color);
                self.drag = Some(Drag::Freehand {
                    last: (x, y),
                    diameter,
                    color,
                    before,
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
            Tool::Fill => {
                let color = self.color_for(side);
                let before = self.bitmap.clone();
                if fill::flood_fill(&mut self.bitmap, x, y, color) {
                    self.history.record(before);
                    self.bump();
                }
            }
            Tool::Picker => self.pick(x, y, side),
        }
    }

    /// Extends an in-progress action to `(x, y)`.
    pub fn extend(&mut self, x: i32, y: i32) {
        match &mut self.drag {
            Some(Drag::Freehand {
                last,
                diameter,
                color,
                ..
            }) => {
                if *last != (x, y) {
                    raster::line(&mut self.bitmap, *last, (x, y), *diameter, *color);
                    *last = (x, y);
                    self.bump();
                }
            }
            Some(Drag::Shape { start, side }) => {
                let (start, side) = (*start, *side);
                self.preview = Some(self.preview_of(start, (x, y), side));
            }
            None => {}
        }
    }

    /// Finishes an action, committing it to the history as one step.
    pub fn end(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Freehand { before, .. } => {
                if self.bitmap != before {
                    self.history.record(before);
                }
            }
            Drag::Shape { start, side } => {
                let end = self.preview.map(|p| p.to).unwrap_or(start);
                let before = self.bitmap.clone();
                let color = self.color_for(side);
                let diameter = self.size;
                match self.tool {
                    Tool::Line => raster::line(&mut self.bitmap, start, end, diameter, color),
                    Tool::Rectangle => {
                        raster::rectangle(&mut self.bitmap, start, end, diameter, color)
                    }
                    Tool::Ellipse => raster::ellipse(&mut self.bitmap, start, end, diameter, color),
                    _ => {}
                }
                if self.bitmap != before {
                    self.history.record(before);
                    self.bump();
                }
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

    /// Samples the pixel at `(x, y)` into the given side.
    pub fn pick(&mut self, x: i32, y: i32, side: Side) {
        let Some(pixel) = self.bitmap.get(x, y) else {
            return;
        };
        match side {
            Side::Primary => self.primary = pixel,
            Side::Secondary => self.secondary = pixel,
        }
    }

    /// Undoes the last committed action.
    pub fn undo(&mut self) {
        if let Some(previous) = self.history.undo(&self.bitmap) {
            self.bitmap = previous;
            self.preview = None;
            self.drag = None;
            self.bump();
        }
    }

    /// Redoes the last undone action.
    pub fn redo(&mut self) {
        if let Some(next) = self.history.redo(&self.bitmap) {
            self.bitmap = next;
            self.preview = None;
            self.drag = None;
            self.bump();
        }
    }

    /// Clears the canvas to white as one undo step.
    pub fn clear(&mut self) {
        let before = self.bitmap.clone();
        self.bitmap.fill(WHITE);
        self.preview = None;
        self.drag = None;
        if self.bitmap != before {
            self.history.record(before);
            self.bump();
        }
    }

    /// Replaces the document with a fresh white bitmap and an empty history.
    pub fn reset(&mut self, width: u32, height: u32) {
        self.bitmap = Bitmap::white(width, height);
        self.history.clear();
        self.preview = None;
        self.drag = None;
        self.bump();
    }

    /// Replaces the bitmap with a decoded one and empties the history.
    pub fn load(&mut self, bitmap: Bitmap) {
        self.bitmap = bitmap;
        self.history.clear();
        self.preview = None;
        self.drag = None;
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

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}
