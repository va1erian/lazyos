//! What `pkgd` keeps in other services: the installed-app rows in `confd`, the
//! open-with registrations in `mimed`, and the "stop this app" call to `init`.
//!
//! Each installed app is one `confd` value at `sys/apps/<system_name>`: the
//! generated `Installed` record (`idl/pkgd.midl`), encoded once here and decoded
//! by `pkgd` and `init`, so there is one definition of the row. `confd` keeps
//! `sys/` writable by root only, which is `pkgd`.

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::client::{CallError, Connection};
use pkgstore::layout;
use user::messenger::client::Native;
use user::messenger::confd::{self, Client as ConfdClient, ValueClient};
use user::messenger::mime::{self, Client as MimeClient};
use user::messenger::pkgd::{wire, Installed};
use user::messenger::services::init::Client as InitClient;
use user::messenger::{errno, services, Error, Result};

use super::peers::retry;

/// The connections and the operations on them.
pub(crate) struct Registry {
    confd: Option<ConfdClient>,
    mimed: Option<MimeClient>,
    init: Option<InitClient>,
}

impl Registry {
    pub(crate) const fn new() -> Registry {
        Registry {
            confd: None,
            mimed: None,
            init: None,
        }
    }

    /// The `confd` client: it resolves on its first call and again after a
    /// restart, so it is built once.
    fn confd(&mut self) -> &ConfdClient {
        self.confd.get_or_insert_with(|| {
            ConfdClient::from_connection(Connection::new(Native, confd::NAME))
        })
    }

    fn mimed(&mut self) -> &MimeClient {
        self.mimed
            .get_or_insert_with(|| MimeClient::from_connection(Connection::new(Native, mime::NAME)))
    }

    /// One installed app, if present.
    pub(crate) fn get(&mut self, system_name: &str) -> Result<Option<Installed>> {
        let key = row_key(system_name)?;
        let client = self.confd();
        let value = retry(|| client.get_value(&key)).map_err(confd::lower)?;
        match value {
            Some(::confd::Value::Bytes(bytes)) => decode_row(&bytes).map(Some),
            Some(_) => Err(Error::Errno(-errno::EINVAL)),
            None => Ok(None),
        }
    }

    /// Record `row`, replacing any earlier row of the same app.
    pub(crate) fn put(&mut self, row: &Installed) -> Result<()> {
        let key = row_key(&row.system_name)?;
        let bytes = wire::encode_installed(row).map_err(Error::Parcel)?;
        let value = ::confd::Value::Bytes(bytes);
        let client = self.confd();
        retry(|| client.set_value(&key, &value)).map_err(confd::lower)
    }

    /// Forget an app; deleting an absent row succeeds.
    pub(crate) fn delete(&mut self, system_name: &str) -> Result<()> {
        let key = row_key(system_name)?;
        let client = self.confd();
        retry(|| client.delete(&key)).map_err(confd::lower)
    }

    /// Every installed app in install order (oldest first). A row that does not
    /// decode is skipped, so one corrupt value cannot hide the others.
    pub(crate) fn list(&mut self) -> Result<Vec<Installed>> {
        let client = self.confd();
        let keys = retry(|| client.list(layout::CONFD_PREFIX)).map_err(confd::lower)?;
        let mut rows = Vec::new();
        for key in keys {
            let Some(name) = layout::system_name_of_key(&key) else {
                continue;
            };
            if let Ok(Some(row)) = self.get(name) {
                if row.system_name == name {
                    rows.push(row);
                }
            }
        }
        rows.sort_by(|a, b| {
            a.installed_at
                .cmp(&b.installed_at)
                .then_with(|| a.system_name.cmp(&b.system_name))
        });
        Ok(rows)
    }

    /// The provisioning stamp of the last pass (`provision::STAMP_KEY`), if
    /// one was stored.
    pub(crate) fn stamp(&mut self) -> Result<Option<String>> {
        let client = self.confd();
        let value =
            retry(|| client.get_value(pkgstore::provision::STAMP_KEY)).map_err(confd::lower)?;
        Ok(match value {
            Some(::confd::Value::Str(text)) => Some(text),
            _ => None,
        })
    }

    /// Record the provisioning stamp.
    pub(crate) fn set_stamp(&mut self, stamp: &str) -> Result<()> {
        let value = ::confd::Value::Str(String::from(stamp));
        let client = self.confd();
        retry(|| client.set_value(pkgstore::provision::STAMP_KEY, &value)).map_err(confd::lower)
    }

    /// Register `app` as the handler of `mime` for `verb`.
    pub(crate) fn mime_register(
        &mut self,
        mime: &str,
        app: &str,
        verb: &str,
        owner: u32,
    ) -> Result<()> {
        let client = self.mimed();
        retry(|| client.register(mime, app, verb, owner)).map_err(lower)
    }

    /// Withdraw `app`'s registration of `mime` for `verb`.
    pub(crate) fn mime_unregister(&mut self, mime: &str, app: &str, verb: &str) -> Result<()> {
        let client = self.mimed();
        retry(|| client.unregister(mime, app, verb)).map_err(lower)
    }

    /// Ask `init` to stop every running instance of `app`; returns how many.
    pub(crate) fn stop(&mut self, app: &str) -> Result<u64> {
        let client = self.init.get_or_insert_with(|| {
            InitClient::from_connection(Connection::new(Native, services::INIT_NAME))
        });
        retry(|| client.stop(app)).map_err(lower)
    }
}

/// A `mimed` failure as `pkgd`'s own error type.
fn lower(error: CallError) -> Error {
    Error::Errno(error.code())
}

/// The `confd` key of `system_name`, refusing a malformed name.
fn row_key(system_name: &str) -> Result<String> {
    layout::confd_key(system_name).map_err(|_| Error::Errno(-errno::EINVAL))
}

/// Decode a stored row.
pub(crate) fn decode_row(bytes: &[u8]) -> Result<Installed> {
    wire::decode_installed(bytes).map_err(Error::Parcel)
}
