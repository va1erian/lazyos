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
