#![forbid(unsafe_code)]

//! Pure explorer logic: no widgets, no I/O, no `xui` types.
//!
//! [`Listing`] loads a directory through a [`Platform`](crate::platform::Platform)
//! and sorts it; [`summarize`] and [`describe`] turn it into the status bar's
//! and the Properties dialog's strings; [`format_size`], [`format_time`],
//! [`title`] and [`is_within`] are the small helpers around them.

mod details;
mod entry;
mod flash;
mod format;
mod path;
mod sort;
mod summary;
mod village;

#[cfg(test)]
mod tests;

pub use details::describe;
pub use entry::{Entry, Listing, SharedListing};
pub use flash::{Clock, FLASH_DURATION, Flash};
pub use format::{format_size, format_time};
pub use path::{deletion_refused, is_root, is_within, title};
pub use summary::summarize;
