//! `confd` as the key-value store of [`xui_host::settings`].

pub use xui_host::settings::user_key;
use xui_host::settings::{KeyValue, Value};
use xui_settings::store::ConfigStore;

use crate::platform::confd_store::ConfdStore;

/// A handle on `confd`; free to copy, so the save worker builds its own.
pub(super) struct ConfdKv;

impl KeyValue for ConfdKv {
    fn get(&self, key: &str) -> Result<Option<Value>, String> {
        ConfdStore::new().try_get(key)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), String> {
        ConfigStore::set(&ConfdStore::new(), key, value)
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        ConfigStore::delete(&ConfdStore::new(), key)
    }

    fn uid(&self) -> Option<u32> {
        ConfigStore::uid(&ConfdStore::new())
    }
}
