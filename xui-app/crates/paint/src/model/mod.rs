#![forbid(unsafe_code)]

//! The pure drawing model: no `Ui`, no backend, no platform.

mod bitmap;
mod fill;
mod history;

mod document;
mod raster;
mod size;
mod tool;

pub use bitmap::{Bitmap, MAX_SIDE, MIN_SIDE, Pixel, WHITE};
pub use document::{Model, Preview, PreviewKind};
pub use history::{DEFAULT_CAP, History};
pub use raster::MAX_BRUSH;
pub use size::parse_size;
pub use tool::{SIZES, Side, Tool};

#[cfg(test)]
mod tests;
