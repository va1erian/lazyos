#![forbid(unsafe_code)]

//! Pure explorer logic: no widgets, no I/O, no `xui` types beyond the view
//! models.
//!
//! [`Listing`] loads a directory through a [`Platform`](crate::platform::Platform)
//! and sorts it by a [`SortOrder`]; [`History`] is a window's Back and
//! Forward; [`resolve_address`] turns the address bar's text into a path;
//! [`summarize`] and [`describe`] turn a listing into the status bar's and
//! the Properties dialog's strings; [`format_size`], [`format_time`],
//! [`title`] and [`is_within`] are the small helpers around them.

mod address;
mod details;
mod entry;
mod format;
mod history;
mod path;
mod sort;
mod summary;
mod village;

#[cfg(test)]
mod tests;

pub use address::resolve_address;
pub use details::describe;
pub use entry::{Entry, Listing, SharedListing, type_name};
pub use format::{format_size, format_time};
pub use history::History;
pub use path::{deletion_refused, is_root, is_within, title};
pub use sort::{SortKey, SortOrder, sort_entries};
pub use summary::summarize;
