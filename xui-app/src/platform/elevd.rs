//! A client of `elevd` (`os.lazy.elevd.v1`, docs/accounts-plan.md U2): the
//! apps' one way to a privileged change. Nothing privileged is handed to the
//! app: it names an operation of `elevd`'s table, the compositor shows the
//! trusted prompt, an administrator approves, and `elevd` performs the
//! operation itself and answers with the result.
//!
//! [`request`] blocks until the prompt is answered (it is modal anyway), so it
//! is bounded generously. On top of it: [`ElevatedConf`], the Config app's
//! store over every key once elevated ([`ConfElevation`]), and
//! [`ElevatingStore`], the Settings app's store that sends `sys/**` writes
//! through `elevd`.

use std::rc::Rc;
use std::sync::Arc;

use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_elevd_v1 as wire;
use xui_confd_editor::store::{
    ConfStore, Elevation, StoreError as ConfStoreError, StoreInfo, Value,
};
use xui_settings::store::{AppChoice, ConfigStore, Detached, StoreError as SettingsError};

use super::confd_store::ConfdStore;
use super::messenger::{CallError, Service};
use crate::sys::errno;

/// `elevd`'s registered name.
const NAME: &str = "os.lazy.elevd";
/// How long a request may take (PIT ticks): the person at the screen has 90 s
/// per prompt, and a wrong password asks again.
const REQUEST_TICKS: u64 = 40_000;
/// The confd subtree only a system service (or `elevd`) writes.
const SYSTEM_PREFIX: &str = "sys/";

/// Ask `elevd` for `operation` with `args`: its detail and values, or the
/// refusal (cancelled, not approved, the service's own error).
pub fn request(operation: &str, args: &[&str]) -> Result<(String, Vec<String>), CallError> {
    request_within(operation, args, REQUEST_TICKS)
}

fn request_within(
    operation: &str,
    args: &[&str],
    ticks: u64,
) -> Result<(String, Vec<String>), CallError> {
    let service = Service::try_connect(NAME).ok_or_else(|| CallError {
        code: -errno::ENOENT,
        message: String::from("the elevation service is not running"),
    })?;
    let body = wire::encode_request_args(&wire::RequestArgs {
        operation: operation.to_string(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    })
    .map_err(|_| CallError {
        code: -errno::EINVAL,
        message: String::from("the request could not be encoded"),
    })?;
    let reply = service.call_detailed_within(
        wire::INTERFACE_ID,
        wire::METHOD_REQUEST,
        ERROR_FIELD,
        body,
        ticks,
    )?;
    let reply = wire::decode_request_reply(&reply.body).map_err(|_| CallError {
        code: -errno::EINVAL,
        message: String::from("elevd's reply could not be read"),
    })?;
    Ok((reply.detail, reply.values))
}

/// A refusal as one line of text.
pub fn describe(error: &CallError) -> String {
    match -error.code {
        errno::ECANCELED => String::from("cancelled"),
        errno::ETIMEDOUT => String::from("nobody answered the administrator prompt"),
        _ => error.message.clone(),
    }
}

/// End this program's standing approvals (best effort).
pub fn release() {
    if let Some(service) = Service::try_connect(NAME) {
        let _ = service.call_within(
            wire::INTERFACE_ID,
            wire::METHOD_RELEASE,
            ERROR_FIELD,
            Vec::new(),
            100,
        );
    }
}

/// A confd value as `elevd`'s `(kind, text)` arguments.
fn value_args(value: &Value) -> (&'static str, String) {
    match value {
        Value::Bool(flag) => ("bool", flag.to_string()),
        Value::I64(number) => ("i64", number.to_string()),
        Value::U64(number) => ("u64", number.to_string()),
        Value::Str(text) => ("str", text.clone()),
        Value::Bytes(bytes) => ("bytes", bytes.iter().map(|b| format!("{b:02x}")).collect()),
    }
}

/// The inverse of [`value_args`], for `conf.get`'s reply.
fn value_of(kind: &str, text: &str) -> Option<Value> {
    Some(match kind {
        "bool" => Value::Bool(text == "true"),
        "i64" => Value::I64(text.parse().ok()?),
        "u64" => Value::U64(text.parse().ok()?),
        "str" => Value::Str(text.to_string()),
        "bytes" => Value::Bytes(
            (0..text.len())
                .step_by(2)
                .map(|at| {
                    text.get(at..at + 2)
                        .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                })
                .collect::<Option<Vec<u8>>>()?,
        ),
        _ => return None,
    })
}

/// Every key, read and written through `elevd` under the approval it holds
/// (a fresh prompt when it ran out). Dropping it (the Config window closed)
/// ends the approval at once, so it does not outlive the elevated view.
pub struct ElevatedConf;

impl Drop for ElevatedConf {
    fn drop(&mut self) {
        release();
    }
}

fn conf_error(error: CallError) -> ConfStoreError {
    match ConfStoreError::from_confd_code(error.code) {
        ConfStoreError::Transport(_) => ConfStoreError::Transport(error.code),
        known => known,
    }
}

impl ConfStore for ElevatedConf {
    fn list(&self, prefix: &str) -> Result<Vec<String>, ConfStoreError> {
        request("conf.list", &[prefix])
            .map(|(_, paths)| paths)
            .map_err(conf_error)
    }

    fn get(&self, path: &str) -> Result<Option<Value>, ConfStoreError> {
        let (_, values) = request("conf.get", &[path]).map_err(conf_error)?;
        match values.as_slice() {
            [] => Ok(None),
            [kind, text] => value_of(kind, text)
                .map(Some)
                .ok_or(ConfStoreError::BadValue),
            _ => Err(ConfStoreError::BadValue),
        }
    }

    fn set(&self, path: &str, value: Value) -> Result<(), ConfStoreError> {
        let (kind, text) = value_args(&value);
        request("conf.set", &[path, kind, &text])
            .map(|_| ())
            .map_err(conf_error)
    }

    fn delete(&self, path: &str) -> Result<(), ConfStoreError> {
        request("conf.delete", &[path])
            .map(|_| ())
            .map_err(conf_error)
    }

    fn info(&self) -> Result<StoreInfo, ConfStoreError> {
        ConfStore::info(&ConfdStore::new())
    }
}

/// The Config app's **Elevate**: one `conf.elevate` approval, then
/// [`ElevatedConf`].
pub struct ConfElevation;

impl Elevation for ConfElevation {
    fn elevate(&self) -> Result<Rc<dyn ConfStore>, String> {
        request("conf.elevate", &[])
            .map(|_| Rc::new(ElevatedConf) as Rc<dyn ConfStore>)
            .map_err(|error| describe(&error))
    }
}

/// The Settings app's store: `confd` for everything, except that writes to
/// `sys/**` (the machine's settings: the keyboard layout, the clock format,
/// the menu) go through `elevd`, which asks an administrator.
pub struct ElevatingStore(pub ConfdStore);

impl ConfigStore for ElevatingStore {
    fn get(&self, key: &str) -> Option<Value> {
        ConfigStore::get(&self.0, key)
    }

    fn set(&self, key: &str, value: Value) -> Result<(), SettingsError> {
        if !key.starts_with(SYSTEM_PREFIX) {
            return ConfigStore::set(&self.0, key, value);
        }
        let (kind, text) = value_args(&value);
        request_within("conf.set", &[key, kind, &text], REQUEST_TICKS)
            .map(|_| ())
            .map_err(|error| describe(&error))
    }

    fn delete(&self, key: &str) -> Result<(), SettingsError> {
        if !key.starts_with(SYSTEM_PREFIX) {
            return ConfigStore::delete(&self.0, key);
        }
        request_within("conf.delete", &[key], REQUEST_TICKS)
            .map(|_| ())
            .map_err(|error| describe(&error))
    }

    fn list(&self, prefix: &str) -> Vec<String> {
        ConfigStore::list(&self.0, prefix)
    }

    fn uid(&self) -> Option<u32> {
        ConfigStore::uid(&self.0)
    }

    fn apps(&self) -> Vec<AppChoice> {
        ConfigStore::apps(&self.0)
    }

    fn persistent(&self) -> bool {
        ConfigStore::persistent(&self.0)
    }

    /// A `sys/**` write waits for an administrator to type their password
    /// (up to [`REQUEST_TICKS`]): the Settings window runs it on a worker,
    /// which resolves `elevd` for itself.
    fn detached(&self) -> Option<Detached> {
        Some(Arc::new(|| {
            Box::new(ElevatingStore(ConfdStore::new())) as Box<dyn ConfigStore>
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_round_trip_through_elevd_arguments() {
        for value in [
            Value::Bool(true),
            Value::I64(-3),
            Value::U64(9),
            Value::Str("a b".into()),
            Value::Bytes(vec![0, 255]),
        ] {
            let (kind, text) = value_args(&value);
            assert_eq!(value_of(kind, &text), Some(value));
        }
        assert_eq!(value_of("float", "1"), None);
    }
}
