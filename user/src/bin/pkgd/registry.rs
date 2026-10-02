//! What `pkgd` keeps in other services: the installed-app rows in `confd`, the
//! open-with registrations in `mimed`, and the "stop this app" call to `init`.
//!
//! Each installed app is one `confd` value at `sys/apps/<system_name>`: the
//! generated `Installed` record (`idl/pkgd.midl`), encoded once here and decoded
//! by `pkgd` and `init`, so there is one definition of the row. `confd` keeps
//! `sys/` writable by root only, which is `pkgd`.

use alloc::string::String;
use alloc::vec::Vec;

use pkgstore::layout;
use user::messenger::confd::{Client as ConfdClient, NAME as CONFD_NAME};
use user::messenger::mime::Client as MimeClient;
use user::messenger::pkgd::{wire, Installed};
use user::messenger::{errno, services, Error, Result};

use super::peers::Peer;

/// The connections and the operations on them.
pub(crate) struct Registry {
    confd: Peer,
    mimed: Peer,
    init: Peer,
}

impl Registry {
    pub(crate) const fn new() -> Registry {
        Registry {
            confd: Peer::new(CONFD_NAME),
            mimed: Peer::new(user::messenger::mime::NAME),
            init: Peer::new(services::INIT_NAME),
        }
    }

    /// One installed app, if present.
    pub(crate) fn get(&mut self, system_name: &str) -> Result<Option<Installed>> {
        let key = row_key(system_name)?;
        let value = self
            .confd
            .run(|endpoint| ConfdClient::from_endpoint(endpoint).get(&key))?;
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
        self.confd
            .run(|endpoint| ConfdClient::from_endpoint(endpoint).set(&key, &value))
    }

    /// Forget an app; deleting an absent row succeeds.
    pub(crate) fn delete(&mut self, system_name: &str) -> Result<()> {
        let key = row_key(system_name)?;
        self.confd
            .run(|endpoint| ConfdClient::from_endpoint(endpoint).delete(&key))
    }

    /// Every installed app in install order (oldest first). A row that does not
    /// decode is skipped, so one corrupt value cannot hide the others.
    pub(crate) fn list(&mut self) -> Result<Vec<Installed>> {
        let keys = self
            .confd
            .run(|endpoint| ConfdClient::from_endpoint(endpoint).list(layout::CONFD_PREFIX))?;
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
        let value = self.confd.run(|endpoint| {
            ConfdClient::from_endpoint(endpoint).get(pkgstore::provision::STAMP_KEY)
        })?;
        Ok(match value {
            Some(::confd::Value::Str(text)) => Some(text),
            _ => None,
        })
    }

    /// Record the provisioning stamp.
    pub(crate) fn set_stamp(&mut self, stamp: &str) -> Result<()> {
        let value = ::confd::Value::Str(String::from(stamp));
        self.confd.run(|endpoint| {
            ConfdClient::from_endpoint(endpoint).set(pkgstore::provision::STAMP_KEY, &value)
        })
    }

    /// Register `app` as the handler of `mime` for `verb`.
    pub(crate) fn mime_register(&mut self, mime: &str, app: &str, verb: &str) -> Result<()> {
        self.mimed
            .run(|endpoint| MimeClient::from_endpoint(endpoint).register(mime, app, verb))
    }

    /// Withdraw `app`'s registration of `mime` for `verb`.
    pub(crate) fn mime_unregister(&mut self, mime: &str, app: &str, verb: &str) -> Result<()> {
        self.mimed
            .run(|endpoint| MimeClient::from_endpoint(endpoint).unregister(mime, app, verb))
    }

    /// Ask `init` to stop every running instance of `app`; returns how many.
    pub(crate) fn stop(&mut self, app: &str) -> Result<u64> {
        self.init.run(|endpoint| services::stop(&endpoint, app))
    }
}

/// The `confd` key of `system_name`, refusing a malformed name.
fn row_key(system_name: &str) -> Result<String> {
    layout::confd_key(system_name).map_err(|_| Error::Errno(-errno::EINVAL))
}

/// Decode a stored row.
pub(crate) fn decode_row(bytes: &[u8]) -> Result<Installed> {
    wire::decode_installed(bytes).map_err(Error::Parcel)
}
