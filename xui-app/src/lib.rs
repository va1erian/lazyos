//! An ordinary `xui` application on LazyOS (issue #114).
//!
//! The crate is a `std` program built for `x86_64-unknown-linux-musl` (static):
//! `xui-core` drives the widgets, the vendored `xui-canvas` paints them with
//! `tiny-skia` + `cosmic-text`, and [`backend::LazyOSBackend`] presents the
//! result through the native display grant (syscall 12) and turns the kernel's
//! input records back into `xui` events. Nothing here links `winit`,
//! `softbuffer` or GL.

pub mod backend;
pub mod font;
pub mod sys;
