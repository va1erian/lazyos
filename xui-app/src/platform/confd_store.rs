//! The [`ConfigStore`] the Settings app and the [`ConfStore`] the Config app
//! use over `confd` (`os.lazy.confd.v1`).
//!
//! Bodies come from the generated `messenger-generated` stubs; only the
//! wire-`Value` <-> `confd::Value` glue lives here. `sys/**` is writable only
//! by uid 0, which is what every desktop app runs as today. The two traits
//! differ in error handling: Settings ignores failures (`Option`/`String`),
//! while Config maps the `CONFD_*` codes onto [`ConfStoreError`] so it can tell
//! `DENIED` from `NOT_FOUND`.

use libmessenger::client::CallError;
use messenger_generated::os_lazy_confd_v1 as wire;
use messenger_generated::os_lazy_init_v1 as init_wire;
use xui_confd_editor::store::{ConfStore, StoreError as ConfStoreError, StoreInfo};
use xui_settings::store::{AppChoice, ConfigStore, StoreError as SettingsError, Value};

use super::messenger::{client, Observed};

/// Reads and writes settings through `confd`. Every call waits at most what
/// `idl/confd.midl` states (a stalled `confd` must not hold the UI thread past
/// it; it commits behind ext2 write-back and a `List` of a deep tree walks it).
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfdStore;

/// A failed call as the negative number the stores report: the errno of a
/// transport failure, a declared store error's code (`1001..`) or an undeclared
/// remote code, negated.
fn code_of(error: &wire::ClientError) -> i64 {
    match error {
        CallError::Transport(errno) => errno.to_syscall(),
        CallError::Remote { code, .. } => -i64::from(*code),
        CallError::Service(declared) => -i64::from(declared.code()),
    }
}

impl ConfdStore {
    pub const fn new() -> ConfdStore {
        ConfdStore
    }

    /// Whether `confd` is registered now: one attempt, never a wait, for a
    /// caller that runs on the defaults rather than block (a browser that starts
    /// before `confd`, the Terminal).
    pub fn registered(&self) -> bool {
        client(wire::Client::try_connect).is_ok()
    }

    /// Run `call` on `confd`'s client, waiting for the service to register
    /// (the stores' callers start with it); an unreachable service is the
    /// connect failure.
    fn with_confd<R>(
        &self,
        call: impl FnOnce(&wire::Client<Observed>) -> Result<R, wire::ClientError>,
    ) -> Result<R, wire::ClientError> {
        let confd = client(wire::Client::connect)?;
        call(&confd)
    }

    /// [`ConfdStore::with_confd`] with the failure as Settings' display text.
    fn call<R>(
        &self,
        call: impl FnOnce(&wire::Client<Observed>) -> Result<R, wire::ClientError>,
    ) -> Result<R, SettingsError> {
        self.with_confd(call).map_err(|error| match error {
            CallError::Transport(_) => format!("confd unavailable ({})", code_of(&error)),
            _ => format!("confd error {}", code_of(&error)),
        })
    }

    /// The same call with the code kept, so the Config app can map the
    /// `CONFD_*` codes onto [`ConfStoreError`] instead of a display string.
    fn call_raw<R>(
        &self,
        call: impl FnOnce(&wire::Client<Observed>) -> Result<R, wire::ClientError>,
    ) -> Result<R, ConfStoreError> {
        self.with_confd(call)
            .map_err(|error| ConfStoreError::from_confd_code(code_of(&error)))
    }
}

/// `confd::Value` -> the generated wire value.
pub fn to_wire(value: &Value) -> wire::Value {
    let mut out = wire::Value::default();
    match value {
        Value::Bool(flag) => {
            out.kind = wire::KIND_BOOL;
            out.bool_value = Some(*flag);
        }
        Value::I64(number) => {
            out.kind = wire::KIND_I64;
            out.i64_value = Some(*number);
        }
        Value::U64(number) => {
            out.kind = wire::KIND_U64;
            out.u64_value = Some(*number);
        }
        Value::Str(text) => {
            out.kind = wire::KIND_STR;
            out.str_value = Some(text.clone());
        }
        Value::Bytes(bytes) => {
            out.kind = wire::KIND_BYTES;
            out.bytes_value = Some(bytes.clone());
        }
    }
    out
}

/// The generated wire value -> `confd::Value`; a kind without its payload is
/// rejected rather than turned into an empty value.
pub fn from_wire(value: &wire::Value) -> Option<Value> {
    match value.kind {
        wire::KIND_BOOL => value.bool_value.map(Value::Bool),
        wire::KIND_I64 => value.i64_value.map(Value::I64),
        wire::KIND_U64 => value.u64_value.map(Value::U64),
        wire::KIND_STR => value.str_value.clone().map(Value::Str),
        wire::KIND_BYTES => value.bytes_value.clone().map(Value::Bytes),
        _ => None,
    }
}

impl ConfdStore {
    /// `Ok(None)` for a key that is not there, `Err` for a read that failed
    /// (a timeout, a refusal): [`ConfigStore::get`] cannot tell them apart.
    pub fn try_get(&self, key: &str) -> Result<Option<Value>, SettingsError> {
        match self.call(|confd| confd.get(key))? {
            None => Ok(None),
            Some(wire) => from_wire(&wire)
                .map(Some)
                .ok_or_else(|| format!("confd value of {key} is malformed")),
        }
    }
}

impl ConfigStore for ConfdStore {
    fn get(&self, key: &str) -> Option<Value> {
        self.try_get(key).ok().flatten()
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SettingsError> {
        self.call(|confd| confd.set(key, &to_wire(&value)))
    }

    fn delete(&self, key: &str) -> Result<(), SettingsError> {
        self.call(|confd| confd.delete(key))
    }

    /// The paths under `prefix` this task may read; empty on any failure.
    fn list(&self, prefix: &str) -> Vec<String> {
        self.call(|confd| confd.list(prefix)).unwrap_or_default()
    }

    /// This task's kernel-stamped uid: confd checks `user/<uid>/**` writes
    /// against the same one.
    fn uid(&self) -> Option<u32> {
        crate::sys::cred_get(None).ok().map(|cred| cred.uid)
    }

    /// The apps `init` can launch (its registry); empty when `init` cannot be
    /// reached, in which case the menu editor only reorders and renames.
    fn apps(&self) -> Vec<AppChoice> {
        let Ok(init) = client(init_wire::Client::connect) else {
            return Vec::new();
        };
        init.list_apps()
            .map(|apps| {
                apps.into_iter()
                    .map(|app| AppChoice {
                        desktop: app.installed || !app.category.is_empty(),
                        id: app.id,
                        name: app.name,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether confd chose a persistent directory, as it reports itself: it
    /// falls back to the ramfs `/tmp/confd` when a persistent one is missing,
    /// read-only or fails its probe write, which a directory check cannot see.
    /// An unreachable confd counts as not persistent (writes fail anyway).
    fn persistent(&self) -> bool {
        self.call(|confd| confd.info())
            .is_ok_and(|info| info.persistent)
    }
}

/// The Config app's richer view of the same service: `List`/`Info` and the
/// `CONFD_*` error codes preserved, so the editor can tell `DENIED` from an
/// absent key and show a read-only state instead of a crash.
impl ConfStore for ConfdStore {
    fn list(&self, prefix: &str) -> Result<Vec<String>, ConfStoreError> {
        self.call_raw(|confd| confd.list(prefix))
    }

    fn get(&self, path: &str) -> Result<Option<Value>, ConfStoreError> {
        let value = self.call_raw(|confd| confd.get(path))?;
        Ok(value.as_ref().and_then(from_wire))
    }

    fn set(&self, path: &str, value: Value) -> Result<(), ConfStoreError> {
        self.call_raw(|confd| confd.set(path, &to_wire(&value)))
    }

    fn delete(&self, path: &str) -> Result<(), ConfStoreError> {
        self.call_raw(|confd| confd.delete(path))
    }

    fn info(&self) -> Result<StoreInfo, ConfStoreError> {
        let info = self.call_raw(|confd| confd.info())?;
        Ok(StoreInfo {
            store_dir: info.store_dir,
            persistent: info.persistent,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_value_kind_round_trips() {
        for value in [
            Value::Bool(true),
            Value::I64(-5),
            Value::U64(0xFF00FF),
            Value::Str("light".into()),
            Value::Bytes(vec![1, 2, 3]),
        ] {
            assert_eq!(from_wire(&to_wire(&value)), Some(value));
        }
    }

    #[test]
    fn the_info_reply_round_trips() {
        for persistent in [true, false] {
            let body = wire::encode_info_reply(&wire::InfoReply {
                store_dir: "/conf".to_owned(),
                persistent,
            })
            .unwrap();
            let info = wire::decode_info_reply(&body).unwrap();
            assert_eq!(info.store_dir, "/conf");
            assert_eq!(info.persistent, persistent);
        }
    }

    #[test]
    fn a_kind_without_its_payload_is_rejected() {
        let mut v = wire::Value {
            kind: wire::KIND_STR,
            ..wire::Value::default()
        };
        assert_eq!(from_wire(&v), None);
        v.kind = 99;
        assert_eq!(from_wire(&v), None);
    }
}
