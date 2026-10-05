#![forbid(unsafe_code)]

//! `lazyarc`: the archive formats behind the LazyOS Archiver
//! (`docs/archiver-plan.md`).
//!
//! An archive is untrusted input. Parsers never pre-allocate from a size or a
//! count the archive states, every entry path is normalised before anyone
//! sees it ([`entry`]), and extraction refuses to write outside its
//! destination ([`safety`]).
//!
//! The library is organised by what a caller does:
//!
//! - [`Archive::open`] detects the [`Format`] and lists the [`Entry`]s;
//!   [`Archive::visit`] streams the wanted entries' data in archive order.
//! - [`extract::extract`] writes a selection to a folder; [`extract::test`]
//!   decompresses everything and checks every checksum.
//! - [`create::create`] builds a new archive from files and folders;
//!   [`rewrite::rewrite`] adds to or deletes from an existing one, through a
//!   temporary sibling renamed over it only on success.
//!
//! Long operations take a [`Progress`]: it counts bytes, names the current
//! entry, and carries the cancel flag a UI sets from another thread.

pub mod archive;
pub mod codec;
pub mod create;
pub mod entry;
pub mod error;
pub mod extract;
pub mod format;
pub mod fuzz;
pub mod pipe;
pub mod progress;
pub mod rewrite;
pub mod safety;
pub mod sevenz;
pub mod single;
pub mod source;
pub mod tar;
pub mod time;
pub mod writer;
pub mod zip;

pub use archive::Archive;
pub use entry::{Entry, EntryKind};
pub use error::{Error, Result};
pub use format::{Format, Level};
pub use progress::Progress;
pub use source::Source;
