#![forbid(unsafe_code)]

//! The pure drawing model: no `Ui`, no backend, no platform.

mod bitmap;
mod blend;
mod damage;
mod fill;
mod history;
mod image;
mod rect;

mod document;
mod raster;
mod size;
mod tool;

pub use bitmap::{Bitmap, MAX_SIDE, MIN_SIDE, Pixel, WHITE};
pub use blend::{div255, source_over};
pub use damage::Damage;
pub use document::{Model, Preview, PreviewKind};
pub use history::{DEFAULT_CAP, Edit, History, LayerCommand, UndoCapture};
pub use image::{
    BACKGROUND, BudgetError, Document, Layer, LayerProps, MEMORY_BUDGET, Reserved, check_budget,
    required_bytes,
};
pub use raster::MAX_BRUSH;
pub use rect::Rect;
pub use size::parse_size;
pub use tool::{SIZES, Side, Tool};

#[cfg(test)]
mod tests;
