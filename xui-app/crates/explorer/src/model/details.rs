#![forbid(unsafe_code)]

//! The Properties dialog's label/value rows.

use super::format_size;
use super::format_time;
use crate::platform::{Kind, Meta};
use std::time::SystemTime;

/// Shown for an unknown value.
const UNKNOWN: &str = "—";

/// Builds the Properties rows for `meta`: name, kind, location, size (a folder
/// shows its direct-entry count, never a recursive size), modified and created
/// times (UTC) and the read-only flag.
///
/// Pure: the same metadata always yields the same rows, so the dialog is a
/// straight render of this vector.
pub fn describe(meta: &Meta) -> Vec<(String, String)> {
    vec![
        ("Name".to_string(), name_of(meta)),
        ("Kind".to_string(), meta.kind.label().to_string()),
        ("Location".to_string(), location_of(meta)),
        ("Size".to_string(), size_of(meta)),
        ("Modified (UTC)".to_string(), time_of(meta.modified)),
        ("Created (UTC)".to_string(), time_of(meta.created)),
        (
            "Read-only".to_string(),
            if meta.readonly { "Yes" } else { "No" }.to_string(),
        ),
    ]
}

/// The lossily displayed name, or [`UNKNOWN`] when the metadata carries none.
fn name_of(meta: &Meta) -> String {
    if meta.name.is_empty() {
        UNKNOWN.to_string()
    } else {
        meta.name.to_string_lossy().into_owned()
    }
}

/// The parent path, or [`UNKNOWN`] for a root (which has no parent).
fn location_of(meta: &Meta) -> String {
    match &meta.parent {
        Some(parent) => parent.display().to_string(),
        None => UNKNOWN.to_string(),
    }
}

/// A file's size, or a directory's direct-entry count. Never walks a directory.
fn size_of(meta: &Meta) -> String {
    match meta.kind {
        Kind::Dir => match meta.entries {
            Some(1) => "1 item".to_string(),
            Some(count) => format!("{count} items"),
            None => UNKNOWN.to_string(),
        },
        Kind::File | Kind::Symlink => meta
            .size
            .map(format_size)
            .unwrap_or_else(|| UNKNOWN.to_string()),
    }
}

/// A UTC timestamp, or [`UNKNOWN`] when the platform has none.
fn time_of(time: Option<SystemTime>) -> String {
    time.map(format_time).unwrap_or_else(|| UNKNOWN.to_string())
}
