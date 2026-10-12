#![forbid(unsafe_code)]

//! The pure drawing model: no `Ui`, no backend, no platform.

mod bitmap;
mod blend;
mod damage;
mod fill;
mod format;
mod history;
mod image;
mod ops;
mod rect;
mod selection;

mod document;
mod size;
mod tool;

pub mod text;
pub mod tools;

pub use bitmap::{Bitmap, MAX_SIDE, MIN_SIDE, Pixel, WHITE};
pub use blend::{div255, source_over};
pub use damage::Damage;
pub use document::{Model, TextKey};
pub use format::{Format, decode_picture, needs_flatten, open as open_file, save as save_file};
pub use history::{DEFAULT_CAP, Edit, History, LayerCommand, UndoCapture};
pub use image::{
    BACKGROUND, BudgetError, Document, Layer, LayerProps, MEMORY_BUDGET, Reserved, check_budget,
    required_bytes,
};
pub use ops::{
    Adjustment, Anchor, CanvasFill, Coverage, Flip, Resample, Turn, black_and_white, box_fit,
    brightness_contrast, flipped_h, flipped_v, hue_saturation, invert, map_rect, map_rect_masked,
    placed, resample, rotated_180, rotated_ccw, rotated_cw, sepia,
};
pub use rect::Rect;
pub(crate) use selection::mix_by;
pub use selection::{CombineMode, Edge, Selection};
pub use size::parse_size;
pub use tool::{Mods, SIZES, Side, Tool};

#[cfg(test)]
mod tests;
