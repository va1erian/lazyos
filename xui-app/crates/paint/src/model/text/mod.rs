#![forbid(unsafe_code)]

//! The Text tool's model (plan P4): the rasterizer contract, the editing
//! state and, behind the crate's `text` feature, the real rasterizer.
//!
//! Everything but [`CosmicRasterizer`] is plain data and logic, so the
//! editing rules are tested with a fake rasterizer and the default build
//! carries no font stack.

#[cfg(feature = "text")]
mod cosmic;
mod edit;
mod style;

#[cfg(all(test, feature = "text"))]
mod cosmic_tests;
#[cfg(test)]
mod tests;

#[cfg(feature = "text")]
pub use cosmic::CosmicRasterizer;
pub use edit::{MAX_CHARS, TextEdit};
pub use style::{LineGeometry, TextAlign, TextCoverage, TextRasterizer, TextStyle};
