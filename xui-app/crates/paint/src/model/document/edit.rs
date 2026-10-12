#![forbid(unsafe_code)]

//! The tool actions: begin/extend/end. A child of `model` so it can touch the
//! document's private fields. Every drawing tool paints into the active layer
//! through the selection and records one history entry per action; the tools
//! themselves are in [`strokes`](super::strokes),
//! [`shape_drag`](super::shape_drag), [`fills`](super::fills) and
//! [`text`](super::text).

use super::{Drag, Model, Pixel, Side, Tool};
use crate::model::history::UndoCapture;
use crate::model::{Edit, Mods};

impl Model {
    /// Selects a tool. A text block is kept as pixels and pixels being placed
    /// (a lift or a paste) land first, unless the new tool is Move Selected
    /// Pixels, which keeps placing them.
    pub fn set_tool(&mut self, tool: Tool) {
        self.end();
        self.commit_text();
        if tool != Tool::MovePixels {
            self.commit_float();
        }
        if tool != self.tool {
            self.prev_tool = self.tool;
        }
        self.tool = tool;
        self.drag = None;
    }

    /// The tool that was active before the current one (where the Color
    /// Picker returns to).
    pub fn previous_tool(&self) -> Tool {
        self.prev_tool
    }

    /// Sets the primary colour.
    pub fn set_primary(&mut self, color: Pixel) {
        self.primary = color;
        self.refresh_text();
    }

    /// Sets the secondary colour.
    pub fn set_secondary(&mut self, color: Pixel) {
        self.secondary = color;
    }

    /// Exchanges the primary and secondary colours.
    pub fn swap_colors(&mut self) {
        std::mem::swap(&mut self.primary, &mut self.secondary);
        self.refresh_text();
    }

    /// The paint colour for a side: the primary (usually the left button) or
    /// the secondary (the right).
    pub fn color_for(&self, side: Side) -> Pixel {
        match side {
            Side::Primary => self.primary,
            Side::Secondary => self.secondary,
        }
    }

    /// Begins an action at document pixel `(x, y)` with no modifier held.
    pub fn begin(&mut self, x: i32, y: i32, side: Side) {
        self.begin_with(x, y, side, Mods::NONE);
    }

    /// Begins an action at document pixel `(x, y)`. The modifiers matter to
    /// the selection tools and the Pencil.
    pub fn begin_with(&mut self, x: i32, y: i32, side: Side, mods: Mods) {
        // A press while a drag is live (its release was lost) finishes that
        // drag first, so its stroke is recorded rather than overwritten.
        self.end();
        match self.tool {
            Tool::RectSelect | Tool::EllipseSelect | Tool::Lasso | Tool::MagicWand => {
                self.begin_select(x, y, side, mods);
            }
            Tool::MovePixels | Tool::MoveSelection => self.begin_move(x, y),
            Tool::Pencil | Tool::Brush | Tool::Eraser => self.begin_stroke(x, y, side, mods),
            Tool::Line | Tool::Rectangle | Tool::RoundedRectangle | Tool::Ellipse => {
                self.begin_shape(x, y, side);
            }
            Tool::Gradient => self.begin_gradient(x, y, side),
            Tool::Fill => self.fill_at(x, y, side),
            Tool::Picker => self.pick_at(x, y, side),
            Tool::Text => self.begin_text(x, y),
            // The view's tools: the canvas zooms and pans, the picture stays.
            Tool::Zoom | Tool::Pan => {}
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
            Drag::Stroke(stroke) => Drag::Stroke(self.extend_stroke(stroke, x, y)),
            Drag::Shape(shape) => Drag::Shape(self.extend_shape(shape, x, y, mods)),
            Drag::Gradient(gradient) => Drag::Gradient(self.extend_gradient(gradient, x, y)),
            Drag::MoveText { start, from } => {
                self.drag_text(start, from, x, y);
                Drag::MoveText { start, from }
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
            Drag::Stroke(stroke) => self.end_stroke(stroke),
            Drag::Shape(shape) => self.end_shape(shape),
            Drag::Gradient(gradient) => self.end_gradient(gradient),
            Drag::Select { base, .. } | Drag::Lasso { base, .. } => self.end_select(base),
            Drag::MoveOutline { base, .. } => self.end_move_outline(base),
            // The lifted pixels stay in the air until committed, and a moved
            // text block stays editable.
            Drag::MoveFloat { .. } | Drag::MoveText { .. } => {}
        }
    }

    /// Abandons an in-progress action (Esc): a stroke, shape or gradient
    /// leaves the layer as it found it and records nothing. Selection and
    /// move drags finish as `end` does.
    pub fn cancel(&mut self) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Stroke(stroke) => self.cancel_stroke(stroke),
            Drag::Shape(shape) => self.cancel_shape(shape),
            Drag::Gradient(gradient) => self.cancel_gradient(gradient),
            Drag::MoveText { start, from } => self.drag_text(start, from, start.0, start.1),
            other => {
                self.drag = Some(other);
                self.end();
            }
        }
    }

    /// Whether a drag is currently in progress.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Samples the pixel at `(x, y)` into the given side, as the Color Picker
    /// does with its current options.
    pub fn pick(&mut self, x: i32, y: i32, side: Side) {
        self.pick_at(x, y, side);
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

    pub(super) fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }
}
