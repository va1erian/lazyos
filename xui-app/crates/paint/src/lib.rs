#![forbid(unsafe_code)]

//! `xui-paint`: a minimal MS-Paint-style doodler built on [`xui_core`].
//!
//! The crate is layered so the drawing logic runs anywhere `std` does, with no
//! window, no GPU and no file I/O:
//!
//! - [`model`] is pure logic: an RGBA [`model::Bitmap`], stroke rasterization,
//!   iterative flood fill, tools and a byte-bounded undo history. It uses no
//!   widget or backend type, so it is unit-tested with no `Ui`.
//! - [`view`] is the thin xui glue: a [`view::PaintApp`] with a menu bar, a
//!   tool options bar, a tool grid, a custom canvas and a Colors panel, mapping
//!   input events and Paint.NET's keyboard shortcuts to [`view::Msg`] and from
//!   there into the model.
//! - [`storage`] is the optional persistence seam: [`storage::MemoryStorage`]
//!   (the default, used on a toy OS and in tests) and the feature-gated
//!   [`storage::FsStorage`].
//! - [`selftest`] runs a scripted doodle and prints PASS/FAIL, for hosts where
//!   `cargo test` is unavailable.
//!
//! The library depends on `xui-core` only; the host binary picks the backend.
//! Running the library uses no threads, timers, env vars, processes or
//! filesystem (xui gap: G14), and drawing, colours and the tool grid need no
//! text, so they stay usable when no font renders (xui gap: G15; the menus and
//! the options bar do need one, and every menu command also has a key). See
//! `GAPS.md` for the full list.

pub mod model;
pub mod render;
pub mod selftest;
pub mod storage;
pub mod view;
pub mod viewport;

pub use model::{Bitmap, Model, Tool};
pub use storage::{MemoryStorage, Storage};
pub use view::{Msg, Observer, PaintApp};

/// The default canvas width in pixels.
pub const DEFAULT_WIDTH: u32 = 320;
/// The default canvas height in pixels.
pub const DEFAULT_HEIGHT: u32 = 240;
/// The largest canvas side in pixels.
pub const MAX_CANVAS_SIDE: u32 = 16384;
/// The byte cap on the undo/redo history (64 MiB).
pub const HISTORY_BYTES: usize = 64 * 1024 * 1024;
