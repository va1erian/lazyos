//! LazyGolf: a procedural golf course generator and a mid-90s software
//! renderer on xui (docs/golf-course-generator.md).
//!
//! [`gen`] builds an 18-hole [`Course`] from a seed; [`render`] draws it
//! into an indexed framebuffer the way Links 386 did (palette ramps, Bayer
//! dithering, haze through a colour lookup table, billboard trees); [`app`]
//! is the window that flies over it. Everything here is portable: the
//! LazyOS binary and the desktop runner (`examples/golf.rs`) only launch it.

pub mod app;
pub mod course;
pub mod field;
pub mod fly;
pub mod game;
pub mod gen;
mod hud;
mod loading;
pub mod math;
pub mod minimap;
pub mod noise;
pub mod render;
pub mod rng;
pub mod view;

pub use app::{GolfApp, Msg, WINDOW};
pub use course::{Archetype, Course, Hole, Material, Object, ObjectKind, Species};
pub use gen::{generate, Generator, Params, Stage};
