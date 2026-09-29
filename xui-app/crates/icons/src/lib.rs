#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! **Global Village**: a multi-colour vector icon set for xui with a 90s
//! world-music look: chunky ink outlines, bevelled highlights and woven
//! zig-zag trim.
//!
//! It has 36 icons in four [`Category`]s (hardware, files, system and
//! toolkit), each a [`Village`] variant. The crate is optional and depends on
//! [`xui_core`] only. Icons are drawn with [`draw`] through the portable
//! `Canvas` path API, so the software backend renders them with `tiny-skia`
//! and the Win32 backend with Direct2D, at any size and DPI.
//!
//! ```no_run
//! use xui_icons::{Palette, Village, draw};
//! # fn paint(canvas: &mut dyn xui_core::backend::Canvas, rect: xui_core::Rect) {
//! draw(canvas, Village::Network, rect, &Palette::GLOBAL_VILLAGE);
//! # }
//! ```
//!
//! The vendored SVGs, and PNG and `.ico` exports of them, live in `assets/`;
//! `assets/generate.py` compiles the SVGs into `src/data/`.

mod data;
mod draw;
mod shape;
mod tone;

#[cfg(test)]
mod tests;

pub use data::{Category, Village};
pub use draw::{GRID, draw};
pub use shape::{Line, Shape};
pub use tone::{Palette, Tone};
