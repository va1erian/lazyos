#![forbid(unsafe_code)]

//! The Archiver's app core (`docs/archiver-plan.md`): a 7-Zip-style archive
//! manager on xui over the `lazyarc` format library.
//!
//! - [`folder`] is the pure folder view over an archive's flat entry list;
//!   [`cells`] formats it for the list view.
//! - [`job`] runs long operations on a worker thread with a shared progress.
//! - [`app`] holds the window state and mirrors it onto the widgets built by
//!   [`ui`]; [`commands`] is what each message does.
//! - [`drag`] is the bridge the platform's drag-and-drop hooks read, and the
//!   extraction a drag out of the archive runs.
//! - [`host`] is the platform seam (start folder, scratch folder, launcher,
//!   evidence log).

pub mod app;
pub mod cells;
pub mod commands;
pub mod drag;
pub mod folder;
pub mod host;
pub mod job;
pub mod ui;

pub use app::{ArchiverApp, Msg, WINDOW};
pub use drag::DragState;
pub use host::Host;
