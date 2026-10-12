//! About LazyOS: a window with a real-time software 3D renderer.
//!
//! [`mesh`] builds the shapes, [`raster`] fills perspective-correct,
//! per-pixel-shaded triangles into a depth-buffered framebuffer,
//! [`backdrop`] paints the synthwave set and [`scene`] puts them together
//! (chrome shader, wireframe styles, pointer-driven spin). [`app`] is the
//! xui window. Everything is portable: the LazyOS binary and the desktop
//! runner (`examples/about.rs`) only launch it.

pub mod app;
pub mod backdrop;
pub mod math;
pub mod mesh;
pub mod pages;
mod panel;
pub mod raster;
pub mod scene;
pub mod view;

pub use app::{AboutApp, Info, Msg, Report, WINDOW};
