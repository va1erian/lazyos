//! PDF documents for the LazyOS PDF Viewer (docs/pdf-reader-plan.md).
//!
//! A thin layer over `hayro`, a pure-Rust PDF rasterizer: [`Document`] opens
//! a file (with a password when it is encrypted), answers page sizes and
//! metadata, and a [`Renderer`] draws any rectangle of a page at any scale
//! into an RGBA [`Tile`]. The viewer never sees `hayro`'s types, so the
//! renderer can change without touching the app. No UI and no LazyOS calls:
//! everything here builds and is tested on the host.

#![forbid(unsafe_code)]

mod document;
mod render;
mod text;

#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;

pub use document::{Document, Info, OpenError, PageSize};
pub use render::{Renderer, Tile, MAX_TILE_SIDE};
pub use text::{PageText, TextGlyph};
