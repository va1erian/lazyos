#![forbid(unsafe_code)]

//! The drawing tools' building blocks (plan P4), pure and host-tested.
//!
//! Nothing here touches `Model`: a later phase switches the tools over and
//! wires the options bar. The pieces:
//!
//! - [`StrokeBuffer`]: coverage for the stroke in progress, merged with `max`
//!   and composited through `blend.rs`, with an optional selection clip;
//! - brush dabs and segments ([`BrushOptions`]), antialiased [`shapes`], the
//!   Pencil's exact [`StrokeBuffer::pixel_line`], and the Shift
//!   [`constrain`]ts;
//! - [`Gradient`], the Paint Bucket's [`bucket_mask`] and the colour
//!   [`picker`].

mod brush;
mod bucket;
mod coverage;
mod gradient;
mod options;
mod picker;
mod stroke;

pub mod constrain;
pub mod shapes;

#[cfg(test)]
mod tests;

pub use brush::pixel_center;
pub use bucket::{bucket_mask, bucket_mask_in, within_tolerance};
pub use coverage::{Clip, Coverage};
pub use gradient::{Gradient, mix};
pub use options::{
    Arrow, BrushOptions, FillArea, FillMode, FillOptions, GradientKind, GradientOptions,
    MAX_BRUSH_WIDTH, PickerOptions, Sample, SampleSize, ShapeOptions,
};
pub use picker::{pick, sample};
pub use shapes::ShapeCoverage;
pub use stroke::{PaintMode, StrokeBuffer};
