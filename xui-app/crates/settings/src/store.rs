//! The configuration seam: get/set/delete typed values by path.

use std::cell::RefCell;
use std::collections::BTreeMap;

pub use confd::Value;

/// Why a write failed, as text for the status line.
pub type StoreError = String;

/// One launchable app in the OS registry (`init`'s app table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppChoice {
    /// The registry id (what a menu entry launches).
    pub id: String,
    /// The display name.
    pub name: String,
}

/// Typed key/value access. `&self` methods: the LazyOS implementation talks to
/// a service and the test one uses interior mutability, so the UI can share one
/// `Rc<dyn ConfigStore>` between widgets.
pub trait ConfigStore {
    fn get(&self, key: &str) -> Option<Value>;
    fn set(&self, key: &str, value: Value) -> Result<(), StoreError>;
    fn delete(&self, key: &str) -> Result<(), StoreError>;
    /// The paths stored under `prefix` that this caller may read (confd's
    /// `List`); empty when the store cannot list.
    fn list(&self, _prefix: &str) -> Vec<String> {
        Vec::new()
    }
    /// The uid this app runs as, which owns the `user/<uid>/...` keys; `None`
    /// when it cannot be read (per-user pages then stay read-only).
    fn uid(&self) -> Option<u32> {
        None
    }
    /// The registry's apps, for the desktop-menu editor; empty when the
    /// registry cannot be reached (the editor then only reorders and renames).
    fn apps(&self) -> Vec<AppChoice> {
        Vec::new()
    }
    /// Whether values survive a reboot (drives the "not persistent" banner).
    fn persistent(&self) -> bool {
        true
    }
}

/// An in-memory store for tests and previews.
#[derive(Default)]
pub struct MemStore {
    map: RefCell<BTreeMap<String, Value>>,
    /// When set, every write fails with this message.
    pub fail_writes: RefCell<Option<String>>,
    /// What [`ConfigStore::apps`] reports.
    pub apps: RefCell<Vec<AppChoice>>,
    /// What [`ConfigStore::uid`] reports.
    pub uid: RefCell<Option<u32>>,
}

impl MemStore {
    pub fn new() -> MemStore {
        MemStore::default()
    }

    pub fn len(&self) -> usize {
        self.map.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.borrow().is_empty()
    }
}

impl ConfigStore for MemStore {
    fn apps(&self) -> Vec<AppChoice> {
        self.apps.borrow().clone()
    }

    fn get(&self, key: &str) -> Option<Value> {
        self.map.borrow().get(key).cloned()
    }

    /// confd's rule: `prefix` itself and the paths below it.
    fn list(&self, prefix: &str) -> Vec<String> {
        self.map
            .borrow()
            .keys()
            .filter(|key| {
                key.as_str() == prefix
                    || key
                        .strip_prefix(prefix)
                        .is_some_and(|rest| rest.starts_with('/'))
            })
            .cloned()
            .collect()
    }

    fn uid(&self) -> Option<u32> {
        *self.uid.borrow()
    }

    fn set(&self, key: &str, value: Value) -> Result<(), StoreError> {
        if let Some(message) = self.fail_writes.borrow().as_ref() {
            return Err(message.clone());
        }
        self.map.borrow_mut().insert(key.to_owned(), value);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        if let Some(message) = self.fail_writes.borrow().as_ref() {
            return Err(message.clone());
        }
        self.map.borrow_mut().remove(key);
        Ok(())
    }
}
