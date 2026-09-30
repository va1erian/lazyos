//! The Settings app's [`ConfigStore`] over `confd` (`os.lazy.confd.v1`).
//!
//! Bodies come from the generated `messenger-generated` stubs; only the
//! wire-`Value` <-> `confd::Value` glue lives here. `sys/**` is writable only
//! by uid 0, which is what every desktop app runs as today.

use messenger_generated::os_lazy_confd_v1 as wire;
use xui_settings::store::{ConfigStore, StoreError, Value};

use super::messenger::Service;

/// The registered service name (not the interface name).
const NAME: &str = "os.lazy.confd";
/// The structured-error field id `confd` replies with.
const ERROR_FIELD: u16 = 15;

const KIND_BOOL: u32 = 0;
const KIND_I64: u32 = 1;
const KIND_U64: u32 = 2;
const KIND_STR: u32 = 3;
const KIND_BYTES: u32 = 4;

/// Reads and writes settings through `confd`.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfdStore;

impl ConfdStore {
    pub const fn new() -> ConfdStore {
        ConfdStore
    }

    fn call(&self, method: u32, body: Vec<u8>) -> Result<libmessenger::Parcel, StoreError> {
        let service =
            Service::connect(NAME).map_err(|code| format!("confd unavailable ({code})"))?;
        service
            .call(wire::INTERFACE_ID, method, ERROR_FIELD, body)
            .map_err(|code| format!("confd error {code}"))
    }
}

/// `confd::Value` -> the generated wire value.
pub fn to_wire(value: &Value) -> wire::Value {
    let mut out = wire::Value::default();
    match value {
        Value::Bool(flag) => {
            out.kind = KIND_BOOL;
            out.bool_value = Some(*flag);
        }
        Value::I64(number) => {
            out.kind = KIND_I64;
            out.i64_value = Some(*number);
        }
        Value::U64(number) => {
            out.kind = KIND_U64;
            out.u64_value = Some(*number);
        }
        Value::Str(text) => {
            out.kind = KIND_STR;
            out.str_value = Some(text.clone());
        }
        Value::Bytes(bytes) => {
            out.kind = KIND_BYTES;
            out.bytes_value = Some(bytes.clone());
        }
    }
    out
}

/// The generated wire value -> `confd::Value`; a kind without its payload is
/// rejected rather than turned into an empty value.
pub fn from_wire(value: &wire::Value) -> Option<Value> {
    match value.kind {
        KIND_BOOL => value.bool_value.map(Value::Bool),
        KIND_I64 => value.i64_value.map(Value::I64),
        KIND_U64 => value.u64_value.map(Value::U64),
        KIND_STR => value.str_value.clone().map(Value::Str),
        KIND_BYTES => value.bytes_value.clone().map(Value::Bytes),
        _ => None,
    }
}

impl ConfigStore for ConfdStore {
    fn get(&self, key: &str) -> Option<Value> {
        let body = wire::encode_get_args(&wire::GetArgs {
            path: key.to_owned(),
        })
        .ok()?;
        let reply = self.call(wire::METHOD_GET, body).ok()?;
        let decoded = wire::decode_get_reply(&reply.body).ok()?;
        from_wire(&decoded.value?)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), StoreError> {
        let body = wire::encode_set_args(&wire::SetArgs {
            path: key.to_owned(),
            value: to_wire(&value),
        })
        .map_err(|_| String::from("value too large"))?;
        self.call(wire::METHOD_SET, body).map(|_| ())
    }

    fn delete(&self, key: &str) -> Result<(), StoreError> {
        let body = wire::encode_delete_args(&wire::DeleteArgs {
            path: key.to_owned(),
        })
        .map_err(|_| String::from("path too long"))?;
        self.call(wire::METHOD_DELETE, body).map(|_| ())
    }

    /// Whether confd chose a persistent directory, as it reports itself: it
    /// falls back to the ramfs `/tmp/confd` when a persistent one is missing,
    /// read-only or fails its probe write, which a directory check cannot see.
    /// An unreachable confd counts as not persistent (writes fail anyway).
    fn persistent(&self) -> bool {
        self.call(wire::METHOD_INFO, Vec::new())
            .ok()
            .and_then(|reply| wire::decode_info_reply(&reply.body).ok())
            .is_some_and(|info| info.persistent)
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
                store_dir: "/data/confd".to_owned(),
                persistent,
            })
            .unwrap();
            let info = wire::decode_info_reply(&body).unwrap();
            assert_eq!(info.store_dir, "/data/confd");
            assert_eq!(info.persistent, persistent);
        }
    }

    #[test]
    fn a_kind_without_its_payload_is_rejected() {
        let mut v = wire::Value::default();
        v.kind = KIND_STR;
        assert_eq!(from_wire(&v), None);
        v.kind = 99;
        assert_eq!(from_wire(&v), None);
    }
}
