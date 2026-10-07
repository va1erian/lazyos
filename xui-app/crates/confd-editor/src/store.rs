//! The configuration seam: list/get/set/delete/info over `confd`.
//!
//! The trait mirrors the `os.lazy.confd.v1` surface one-for-one, including the
//! structured error codes, so the LazyOS implementation is a thin wire adapter
//! and the tests run against [`MemStore`], which enforces the same path grammar
//! and size limits.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

pub use confd::Value;

/// The largest accepted path, in bytes (the service's own limit).
pub const MAX_PATH_LEN: usize = confd::MAX_PATH_LEN;
/// The largest accepted value payload, in bytes.
pub const MAX_VALUE_LEN: usize = confd::MAX_VALUE_LEN;
/// The largest total `path + value` weight the store may hold.
pub const MAX_STORE_BYTES: usize = confd::MAX_STORE_BYTES;

/// The `CONFD_*` codes carried in a reply's structured error field.
pub const CONFD_NOT_FOUND: i64 = 1001;
pub const CONFD_BAD_PATH: i64 = 1002;
pub const CONFD_TOO_LARGE: i64 = 1003;
pub const CONFD_DENIED: i64 = 1004;
pub const CONFD_IO: i64 = 1005;
pub const CONFD_BAD_VALUE: i64 = 1006;

/// Why a `confd` operation failed, as a value the UI can act on.
///
/// [`StoreError::Denied`] is deliberately distinct from
/// [`StoreError::NotFound`]: the first drives a read-only state, the second is
/// an absent key, which is not an error at all for `get`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum StoreError {
    /// The path is absent (only `delete`/`get` context).
    NotFound,
    /// The path fails the grammar (`validate_path`).
    BadPath,
    /// The value or store exceeds a size limit.
    TooLarge,
    /// The caller's uid may not access the path.
    Denied,
    /// The backing store could not be read or written.
    Io,
    /// A malformed value or reply.
    BadValue,
    /// The service could not be reached, or answered with an unknown code.
    Transport(i64),
}

impl StoreError {
    /// Maps a `CONFD_*` code (positive from the service, or the negated errno
    /// the platform client reports) onto a variant.
    pub fn from_confd_code(code: i64) -> StoreError {
        match code.unsigned_abs() {
            1001 => StoreError::NotFound,
            1002 => StoreError::BadPath,
            1003 => StoreError::TooLarge,
            1004 => StoreError::Denied,
            1005 => StoreError::Io,
            1006 => StoreError::BadValue,
            other => StoreError::Transport(other as i64),
        }
    }

    /// A short, human-readable explanation for the status line.
    pub fn message(&self) -> String {
        match self {
            StoreError::NotFound => "no value is stored at that path".into(),
            StoreError::BadPath => "that is not a valid confd path".into(),
            StoreError::TooLarge => "the value or store exceeds a confd size limit".into(),
            StoreError::Denied => "you may not access that path".into(),
            StoreError::Io => "the confd store could not be read or written".into(),
            StoreError::BadValue => "the value is not valid for that kind".into(),
            StoreError::Transport(code) => format!("confd is unavailable (error {code})"),
        }
    }
}

/// Where the store lives, as `Info()` reports it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoreInfo {
    /// The directory `confd` chose.
    pub store_dir: String,
    /// Whether values survive a reboot (`false` on the ramfs fallback).
    pub persistent: bool,
}

/// Typed key/value access to the configuration registry.
///
/// `&self` methods: the LazyOS implementation talks to a service and the test
/// one uses interior mutability, so the UI can share one `Rc<dyn ConfStore>`.
pub trait ConfStore {
    /// Every path the caller may read under `prefix`, in sorted order.
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError>;
    /// The value at `path`; `Ok(None)` is an absent key, not an error.
    fn get(&self, path: &str) -> Result<Option<Value>, StoreError>;
    /// Create or overwrite `path`.
    fn set(&self, path: &str, value: Value) -> Result<(), StoreError>;
    /// Remove `path`; deleting an absent path succeeds.
    fn delete(&self, path: &str) -> Result<(), StoreError>;
    /// The store directory and whether it is persistent.
    fn info(&self) -> Result<StoreInfo, StoreError>;
}

/// Access beyond the user's own keys (docs/accounts-plan.md U2): every key,
/// `sys/**` writes included, once an administrator approved it on the
/// trusted prompt (`elevd`). Nothing is handed to the app: the returned store
/// asks `elevd`, which performs each read and write itself.
pub trait Elevation {
    /// The store over every key, or the refusal to show.
    fn elevate(&self) -> Result<Rc<dyn ConfStore>, String>;
}

/// What the editor lists, and whether it may ask for more.
#[derive(Clone)]
pub struct Scope {
    /// The prefix listed (`user/<uid>` by default; empty: every key).
    pub prefix: String,
    /// How to ask for every key, when it can.
    pub elevation: Option<Rc<dyn Elevation>>,
}

impl Scope {
    /// Every key the store lets the caller see, with no way to elevate
    /// (tests, previews).
    pub fn everything() -> Scope {
        Scope {
            prefix: String::new(),
            elevation: None,
        }
    }

    /// `uid`'s own keys, elevating through `elevation`.
    pub fn own(uid: u32, elevation: Rc<dyn Elevation>) -> Scope {
        Scope {
            prefix: format!("user/{uid}"),
            elevation: Some(elevation),
        }
    }
}

/// An in-memory store for tests and previews, with the service's own rules.
#[derive(Default)]
pub struct MemStore {
    map: RefCell<BTreeMap<String, Value>>,
    /// When set, every `set`/`delete` fails with this error.
    pub fail_writes: RefCell<Option<StoreError>>,
    /// When set, every `get` fails with this error.
    pub fail_reads: RefCell<Option<StoreError>>,
    /// When set, every `list` fails with this error.
    pub fail_lists: RefCell<Option<StoreError>>,
    /// Simulates a non-root caller: `sys/**` becomes read-only.
    pub read_only_sys: Cell<bool>,
    /// Whether `info()` reports a persistent store.
    pub persistent: Cell<bool>,
    /// The directory `info()` reports.
    pub store_dir: RefCell<String>,
    /// How many `set`/`delete` calls reached the map (even a denied one), so a
    /// test can prove a read-only editor stops calling the store.
    pub writes: Cell<usize>,
}

impl MemStore {
    /// A writable, persistent store with a sensible directory.
    pub fn new() -> MemStore {
        MemStore {
            persistent: Cell::new(true),
            store_dir: RefCell::new(confd::dir::PREFERRED_DIR.into()),
            ..MemStore::default()
        }
    }

    /// The number of stored paths.
    pub fn len(&self) -> usize {
        self.map.borrow().len()
    }

    /// Whether the store holds no paths.
    pub fn is_empty(&self) -> bool {
        self.map.borrow().is_empty()
    }

    /// Seeds a value without the access checks, for test setup.
    pub fn seed(&self, path: &str, value: Value) {
        self.map.borrow_mut().insert(path.to_owned(), value);
    }

    /// Whether `path` is under `prefix` on a segment boundary (mirrors the
    /// service: `sys/net` does not cover `sys/network`).
    fn under_prefix(path: &str, prefix: &str) -> bool {
        if prefix.is_empty() {
            return true;
        }
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl ConfStore for MemStore {
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        if let Some(error) = self.fail_lists.borrow().as_ref() {
            return Err(error.clone());
        }
        if !prefix.is_empty() {
            confd::validate_path(prefix).map_err(|_| StoreError::BadPath)?;
        }
        let map = self.map.borrow();
        Ok(map
            .keys()
            .filter(|path| MemStore::under_prefix(path, prefix))
            .cloned()
            .collect())
    }

    fn get(&self, path: &str) -> Result<Option<Value>, StoreError> {
        if let Some(error) = self.fail_reads.borrow().as_ref() {
            return Err(error.clone());
        }
        confd::validate_path(path).map_err(|_| StoreError::BadPath)?;
        Ok(self.map.borrow().get(path).cloned())
    }

    fn set(&self, path: &str, value: Value) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        if let Some(error) = self.fail_writes.borrow().as_ref() {
            return Err(error.clone());
        }
        confd::validate_path(path).map_err(|_| StoreError::BadPath)?;
        if self.read_only_sys.get() && is_sys(path) {
            return Err(StoreError::Denied);
        }
        if value.size_bytes() > MAX_VALUE_LEN {
            return Err(StoreError::TooLarge);
        }
        let mut map = self.map.borrow_mut();
        let old = map.get(path).map_or(0, |old| path.len() + old.size_bytes());
        let total: usize = map
            .iter()
            .map(|(key, value)| key.len() + value.size_bytes())
            .sum::<usize>()
            .saturating_sub(old)
            .saturating_add(path.len() + value.size_bytes());
        if total > MAX_STORE_BYTES {
            return Err(StoreError::TooLarge);
        }
        map.insert(path.to_owned(), value);
        Ok(())
    }

    fn delete(&self, path: &str) -> Result<(), StoreError> {
        self.writes.set(self.writes.get() + 1);
        if let Some(error) = self.fail_writes.borrow().as_ref() {
            return Err(error.clone());
        }
        confd::validate_path(path).map_err(|_| StoreError::BadPath)?;
        if self.read_only_sys.get() && is_sys(path) {
            return Err(StoreError::Denied);
        }
        self.map.borrow_mut().remove(path);
        Ok(())
    }

    fn info(&self) -> Result<StoreInfo, StoreError> {
        Ok(StoreInfo {
            store_dir: self.store_dir.borrow().clone(),
            persistent: self.persistent.get(),
        })
    }
}

/// Whether `path` is in the world-readable, root-writable `sys/` subtree.
fn is_sys(path: &str) -> bool {
    path == "sys" || path.starts_with("sys/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_delete_round_trip() {
        let store = MemStore::new();
        assert_eq!(store.get("sys/ui/mode"), Ok(None));
        store.set("sys/ui/mode", Value::Str("dark".into())).unwrap();
        assert_eq!(
            store.get("sys/ui/mode"),
            Ok(Some(Value::Str("dark".into())))
        );
        store.delete("sys/ui/mode").unwrap();
        assert_eq!(store.get("sys/ui/mode"), Ok(None));
        // Deleting an absent key is fine.
        assert_eq!(store.delete("sys/ui/mode"), Ok(()));
    }

    #[test]
    fn path_grammar_is_enforced() {
        let store = MemStore::new();
        for bad in ["", "etc/passwd", "sys/../x", "sys/", "sys/a b", "Sys/a"] {
            assert_eq!(store.set(bad, Value::Bool(true)), Err(StoreError::BadPath));
            assert_eq!(store.get(bad), Err(StoreError::BadPath));
        }
    }

    #[test]
    fn list_is_sorted_and_segment_aware() {
        let store = MemStore::new();
        store.seed("sys/ui/mode", Value::Bool(true));
        store.seed("sys/ui2", Value::Bool(true));
        store.seed("sys/net/eth0/mtu", Value::U64(1500));
        assert_eq!(store.list("sys/ui"), Ok(vec!["sys/ui/mode".to_string()]));
        assert_eq!(
            store.list(""),
            Ok(vec![
                "sys/net/eth0/mtu".to_string(),
                "sys/ui/mode".to_string(),
                "sys/ui2".to_string(),
            ])
        );
    }

    #[test]
    fn size_limits_are_enforced() {
        let store = MemStore::new();
        let big = "a".repeat(MAX_VALUE_LEN + 1);
        assert_eq!(
            store.set("sys/big", Value::Str(big)),
            Err(StoreError::TooLarge)
        );
        let exact = "a".repeat(MAX_VALUE_LEN);
        assert_eq!(store.set("sys/ok", Value::Str(exact)), Ok(()));
    }

    #[test]
    fn read_only_sys_denies_writes_but_allows_reads() {
        let store = MemStore::new();
        store.seed("sys/ui/mode", Value::Str("dark".into()));
        store.read_only_sys.set(true);
        assert_eq!(
            store.get("sys/ui/mode"),
            Ok(Some(Value::Str("dark".into())))
        );
        assert_eq!(
            store.set("sys/ui/mode", Value::Str("light".into())),
            Err(StoreError::Denied)
        );
        assert_eq!(store.delete("sys/ui/mode"), Err(StoreError::Denied));
    }

    #[test]
    fn fail_switches_surface_the_error_and_leave_state() {
        let store = MemStore::new();
        store.seed("sys/a", Value::Bool(true));
        *store.fail_writes.borrow_mut() = Some(StoreError::Denied);
        assert_eq!(
            store.set("sys/a", Value::Bool(false)),
            Err(StoreError::Denied)
        );
        assert_eq!(store.get("sys/a"), Ok(Some(Value::Bool(true))));
        *store.fail_reads.borrow_mut() = Some(StoreError::Io);
        assert_eq!(store.get("sys/a"), Err(StoreError::Io));
        *store.fail_lists.borrow_mut() = Some(StoreError::Denied);
        assert_eq!(store.list(""), Err(StoreError::Denied));
    }

    #[test]
    fn codes_map_to_variants_in_both_signs() {
        for (code, error) in [
            (CONFD_NOT_FOUND, StoreError::NotFound),
            (CONFD_BAD_PATH, StoreError::BadPath),
            (CONFD_TOO_LARGE, StoreError::TooLarge),
            (CONFD_DENIED, StoreError::Denied),
            (CONFD_IO, StoreError::Io),
            (CONFD_BAD_VALUE, StoreError::BadValue),
        ] {
            assert_eq!(StoreError::from_confd_code(code), error);
            assert_eq!(StoreError::from_confd_code(-code), error);
        }
        assert_eq!(StoreError::from_confd_code(42), StoreError::Transport(42));
    }

    #[test]
    fn writes_counter_counts_attempts() {
        let store = MemStore::new();
        store.read_only_sys.set(true);
        assert_eq!(store.writes.get(), 0);
        let _ = store.set("sys/a", Value::Bool(true));
        assert_eq!(store.writes.get(), 1);
    }
}
