//! In-memory path tree, uid access rules and change records.
//!
//! Every mutating call returns a [`Change`] the `regd` service publishes
//! after the write has been persisted, so subscribers only ever see committed
//! states. Rejecting a call never leaves a partial mutation behind: all
//! checks run before the map is touched.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::path::{can_read, can_write, validate_path};
use crate::value::Value;
use crate::{MAX_STORE_BYTES, MAX_VALUE_LEN};

/// The caller identity a Messenger request carries (kernel-stamped).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caller {
    /// Effective user id of the calling process.
    pub uid: u32,
}

/// A committed mutation, for the caller to persist and publish.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    /// The path that changed.
    pub path: String,
    /// The new value, or `None` when the path was deleted.
    pub new: Option<Value>,
}

/// Why a store operation was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The path is not valid; see [`validate_path`].
    BadPath,
    /// The value, or the store after the change, exceeds a size limit.
    TooLarge,
    /// The caller's uid does not permit this operation on this path.
    Denied,
}

impl Error {
    /// A short, human-readable explanation (friendly-errors convention).
    pub const fn message(self) -> &'static str {
        match self {
            Error::BadPath => "the path is not a valid regd path",
            Error::TooLarge => "the value or store exceeds a regd size limit",
            Error::Denied => "the caller may not access this path",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

/// The whole configuration tree, one [`Value`] per path.
///
/// The fields are private so `total_bytes` can never drift from `entries`;
/// the only ways in are [`Store::set`], [`Store::delete`] and the crate's
/// decoder.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Store {
    entries: BTreeMap<String, Value>,
    total_bytes: usize,
}

impl Store {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of stored paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no paths.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the value at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::BadPath`] or [`Error::Denied`]. A denied path is never
    /// reported as absent, so callers cannot probe another user's subtree.
    pub fn get(&self, path: &str, caller: Caller) -> Result<Option<&Value>, Error> {
        validate_path(path)?;
        if !can_read(path, caller.uid) {
            return Err(Error::Denied);
        }
        Ok(self.entries.get(path))
    }

    /// Creates or overwrites the value at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::BadPath`], [`Error::Denied`], or [`Error::TooLarge`] when the
    /// value or the resulting store exceeds its limit. On error the store is
    /// unchanged.
    pub fn set(&mut self, path: &str, value: Value, caller: Caller) -> Result<Change, Error> {
        validate_path(path)?;
        if !can_write(path, caller.uid) {
            return Err(Error::Denied);
        }
        if value.size_bytes() > MAX_VALUE_LEN {
            return Err(Error::TooLarge);
        }
        let new_entry = path.len() + value.size_bytes();
        let old_entry = self
            .entries
            .get(path)
            .map_or(0, |old| path.len() + old.size_bytes());
        let total = self
            .total_bytes
            .checked_sub(old_entry)
            .and_then(|total| total.checked_add(new_entry))
            .ok_or(Error::TooLarge)?;
        if total > MAX_STORE_BYTES {
            return Err(Error::TooLarge);
        }
        self.entries.insert(String::from(path), value.clone());
        self.total_bytes = total;
        Ok(Change {
            path: String::from(path),
            new: Some(value),
        })
    }

    /// Removes the value at `path`.
    ///
    /// # Errors
    ///
    /// [`Error::BadPath`] or [`Error::Denied`]. Deleting an absent path is
    /// `Ok(None)` — access is checked first so denial still wins.
    pub fn delete(&mut self, path: &str, caller: Caller) -> Result<Option<Change>, Error> {
        validate_path(path)?;
        if !can_write(path, caller.uid) {
            return Err(Error::Denied);
        }
        match self.entries.remove(path) {
            Some(value) => {
                let removed = path.len() + value.size_bytes();
                self.total_bytes = self.total_bytes.saturating_sub(removed);
                Ok(Some(Change {
                    path: String::from(path),
                    new: None,
                }))
            }
            None => Ok(None),
        }
    }

    /// Lists the paths under `prefix` (or equal to it) that `caller` may
    /// read, in sorted order.
    ///
    /// An empty prefix means the whole store. A non-empty prefix must itself
    /// be a valid path and matches on segment boundaries, so `sys/net` does
    /// not list `sys/network`. The prefix itself is not access-checked: each
    /// returned path is filtered individually, so a caller sees nothing it
    /// could not [`Store::get`].
    ///
    /// # Errors
    ///
    /// [`Error::BadPath`] when a non-empty prefix is invalid.
    pub fn list(&self, prefix: &str, caller: Caller) -> Result<Vec<&str>, Error> {
        if !prefix.is_empty() {
            validate_path(prefix)?;
        }
        let mut paths = Vec::new();
        for path in self.entries.keys() {
            if !under_prefix(path, prefix) || !can_read(path, caller.uid) {
                continue;
            }
            paths.push(path.as_str());
        }
        Ok(paths)
    }

    /// Iterates all entries. Crate-internal because `decode` needs the map
    /// order for a deterministic encoding; the service API is `get`/`list`.
    pub(crate) fn iter_raw(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .map(|(path, value)| (path.as_str(), value))
    }

    /// Inserts an entry `decode` has already validated, keeping
    /// `total_bytes` in sync. Returns `false` when the path already exists,
    /// so the caller can reject duplicate entries instead of silently
    /// overwriting.
    ///
    /// `decode` is the only caller and has already rejected the store if the
    /// running sum would pass [`MAX_STORE_BYTES`]; the saturating add only
    /// stops a future misuse from wrapping, and the debug assertion catches
    /// one in tests.
    pub(crate) fn insert_decoded(&mut self, path: String, value: Value) -> bool {
        if self.entries.contains_key(&path) {
            return false;
        }
        let entry = path.len() + value.size_bytes();
        debug_assert!(self
            .total_bytes
            .checked_add(entry)
            .is_some_and(|total| total <= MAX_STORE_BYTES));
        self.total_bytes = self.total_bytes.saturating_add(entry);
        self.entries.insert(path, value);
        true
    }
}

/// Segment-aware prefix match: `sys/net` covers `sys/net` and `sys/net/...`,
/// but not `sys/network`.
fn under_prefix(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}
