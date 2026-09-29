#![forbid(unsafe_code)]

//! A portable, spatial file explorer for `xui`.
//!
//! One window is one open folder: no tree, no address bar, no navigation inside
//! a window. A folder opens another window (or reuses the one already showing
//! it), double-clicking a file hands it to the [`Launcher`], and the status bar
//! summarises the directory and the current selection.
//!
//! The crate is split into a portable core and a thin shell:
//!
//! - [`platform`] is the OS seam: [`Platform`] (list, metadata, delete, home)
//!   and [`Launcher`] (open a file with the OS handler). Neither trait mentions
//!   `xui`, `std::fs` or a `cfg`; a target such as LazyOS implements them and a
//!   [`Backend`](xui_core::backend::Backend) and nothing else.
//! - [`model`] is pure logic (sorting, summaries, properties, size and time
//!   formatting, path helpers) with no widgets and no I/O.
//! - [`window`] is the per-window [`App`](xui_core::app::App): it owns the
//!   [`IconView`](xui_core::widget::IconView) and [`StatusBar`](xui_core::widget::StatusBar)
//!   and reacts to them through messages.
//! - [`shell`] is the shared state: the platform, the launcher and the window
//!   registry that makes an already-open folder a no-op and lets a delete close
//!   the windows below it.
//! - [`testing`] is an in-memory [`Platform`] for tests that must not touch the
//!   real disk.
//! - `std_platform` (the default `std-platform` feature) is the desktop shell:
//!   `StdPlatform` over `std::fs` and `DesktopLauncher` over the OS opener.

pub mod model;
pub mod platform;
pub mod shell;
pub mod testing;
pub mod window;

#[cfg(feature = "std-platform")]
pub mod std_platform;

pub use model::{Entry, Listing};
pub use platform::{Kind, Launcher, Meta, Platform, RawEntry};
pub use shell::Explorer;
pub use testing::MemPlatform;
pub use window::ExplorerWindow;

#[cfg(feature = "std-platform")]
pub use std_platform::{DesktopLauncher, StdPlatform};
