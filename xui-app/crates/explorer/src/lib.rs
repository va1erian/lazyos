#![forbid(unsafe_code)]

//! A portable, explorer-style file manager for `xui`.
//!
//! A window browses folders in place, like a web browser: opening a folder
//! replaces the view, a [`History`](model::History) backs the Back and Forward
//! buttons, Up goes to the parent, and an address bar takes a typed path. The
//! window title is the folder's name. The folder shows as icon tiles or as a
//! details list with sortable columns; "Open in New Window" in the context
//! menu is the way to a second window. Double-clicking a file hands it to the
//! [`Launcher`], and the status bar summarises the folder and the selection.
//!
//! The crate is split into a portable core and a thin shell:
//!
//! - [`platform`] is the OS seam: [`Platform`] (list, metadata, delete, home)
//!   and [`Launcher`] (open a file with the OS handler). Neither trait mentions
//!   `xui`, `std::fs` or a `cfg`; a target such as LazyOS implements them and a
//!   [`Backend`](xui_core::backend::Backend) and nothing else.
//! - [`model`] is pure logic (sorting, history, the address bar, summaries,
//!   properties, size and time formatting, path helpers) with no widgets and
//!   no I/O.
//! - [`window`] is the per-window [`App`](xui_core::app::App): it owns the
//!   toolbar, the [`IconView`](xui_core::widget::IconView) and
//!   [`ListView`](xui_core::widget::ListView) and the
//!   [`StatusBar`](xui_core::widget::StatusBar), and reacts to them through
//!   messages.
//! - [`shell`] is the shared state: the platform, the launcher, the session
//!   and what each open window shows, so a delete or a paste refreshes every
//!   window it affects.
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
pub use window::{ExplorerWindow, ViewMode, ViewOptions};

#[cfg(feature = "std-platform")]
pub use std_platform::{DesktopLauncher, StdPlatform};
