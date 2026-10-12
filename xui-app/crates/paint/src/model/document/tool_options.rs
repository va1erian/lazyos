#![forbid(unsafe_code)]

//! Each tool's options: what the options bar reads and writes.
//!
//! The width is one number for the Paintbrush, the Eraser and every shape
//! (Paint.NET shares it too), so a thin line stays thin when the tool changes.

use super::Model;
use crate::model::text::TextStyle;
use crate::model::tools::{
    BrushOptions, FillOptions, GradientOptions, MAX_BRUSH_WIDTH, PickerOptions, ShapeOptions,
};

impl Model {
    /// Sets the width of the brush, the eraser and the shapes, clamped to
    /// `1..=MAX_BRUSH_WIDTH`.
    pub fn set_size(&mut self, size: u32) {
        let size = size.clamp(1, MAX_BRUSH_WIDTH);
        self.brush.width = size;
        self.shape.width = size as f32;
    }

    /// The Paintbrush and Eraser options (the width is [`Model::size`]).
    pub fn brush(&self) -> BrushOptions {
        self.brush
    }

    /// Replaces the brush options; the width is shared with the shapes.
    pub fn set_brush(&mut self, brush: BrushOptions) {
        self.brush = brush.clamped();
        self.set_size(self.brush.width);
    }

    /// The Line, Rectangle, Rounded Rectangle and Ellipse options.
    pub fn shape(&self) -> ShapeOptions {
        self.shape
    }

    /// Replaces the shape options; the width is shared with the brush.
    pub fn set_shape(&mut self, shape: ShapeOptions) {
        self.shape = ShapeOptions {
            radius: shape.radius.clamp(0.0, 1000.0),
            ..shape
        };
        self.set_size(shape.width.round().max(1.0) as u32);
    }

    /// The Paint Bucket options.
    pub fn fill(&self) -> FillOptions {
        self.fill
    }

    /// Replaces the Paint Bucket options.
    pub fn set_fill(&mut self, fill: FillOptions) {
        self.fill = FillOptions {
            tolerance: fill.tolerance.min(100),
            ..fill
        };
    }

    /// The Gradient options.
    pub fn gradient(&self) -> GradientOptions {
        self.gradient
    }

    /// Replaces the Gradient options.
    pub fn set_gradient(&mut self, gradient: GradientOptions) {
        self.gradient = gradient;
    }

    /// The Color Picker options.
    pub fn picker(&self) -> PickerOptions {
        self.picker
    }

    /// Replaces the Color Picker options.
    pub fn set_picker(&mut self, picker: PickerOptions) {
        self.picker = picker;
    }

    /// The Text tool's style.
    pub fn text_style(&self) -> &TextStyle {
        &self.text_style
    }

    /// Replaces the Text style; a block being edited takes it at once.
    pub fn set_text_style(&mut self, style: TextStyle) {
        self.text_style = style.clone();
        if let Some(session) = &mut self.text {
            session.edit.style = style;
        }
        self.refresh_text();
    }
}
