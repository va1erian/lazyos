//! Carrying out an approved operation, as `elevd`.
//!
//! Each service accepts these calls from `elevd`'s identity alone
//! (`elevpolicy::is_elevd`), so nothing here hands privilege to the asker:
//! the asker gets the result, never a capability. The call is bounded, and
//! the service's own refusal comes back to the asker as it was given.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use elevpolicy::{value_args, Operation, Value, WifiChange, NET_PREFIX, POWER_PREFIX};
use libmessenger::client::{CallError, Deadline};
use user::messenger::client::Native;
use user::messenger::confd::ValueClient;
use user::messenger::services::init::Client as InitClient;
use user::messenger::{confd, errno, keyd, pkgd, timed};

use super::package::Approved;
use super::{accounts_client, Refusal};

/// How long a configuration call may take (PIT ticks).
const CONFD_TICKS: u64 = 300;
/// [`CONFD_TICKS`] as a call bound.
const CONFD_BOUND: Deadline = Deadline::ticks(CONFD_TICKS);
/// How long an account change may take: Argon2id and a home under TCG.
const ACCOUNT_TICKS: u64 = 9000;
const ACCOUNT_BOUND: Deadline = Deadline::ticks(ACCOUNT_TICKS);
/// Most keys `conf.list` returns.
const MAX_KEYS: usize = 512;

/// What the asker gets: a description and the operation's values.
pub(crate) type Done = (String, Vec<String>);

/// Perform `op`. A package install carries what the prompt described
/// (`package.rs`): `pkgd` installs those bytes or nothing.
pub(crate) fn perform(op: &Operation, package: Option<&Approved>) -> Result<Done, Refusal> {
    match op {
        Operation::PkgInstall { .. }
        | Operation::PkgUpdateCore { .. }
        | Operation::PkgInstallDevtool { .. } => {
            let approved = package.ok_or_else(|| {
                Refusal::new(
                    errno::EINVAL,
                    "the package was not inspected before approval",
                )
            })?;
            // Whether a core app is replaced, as `inspect` found: only
            // `pkg.update-core`, or `pkg.install-devtool` of an update to a
            // core developer tool. What the administrator was shown: only
            // `pkg.install-devtool` led with the developer-tool warning, and
            // `pkgd` installs a tool from no other.
            let devtool = matches!(op, Operation::PkgInstallDevtool { .. });
            let client = pkgd::Client::connect(Native).map_err(Refusal::of_call)?;
            let app = client
                .install_approved(&approved.path, &approved.digest, approved.core, devtool)
                .map_err(Refusal::of_call)?;
            done(format!("Installed {} {}", app.system_name, app.version))
        }
        Operation::PkgRemove { system_name } => {
            let client = pkgd::Client::connect(Native).map_err(Refusal::of_call)?;
            client.remove(system_name).map_err(Refusal::of_call)?;
            done(format!("Removed {system_name}"))
        }
        Operation::ConfSet { path, value } => {
            conf()?
                .set_value_within(path, value, CONFD_BOUND)
                .map_err(confd_refusal)?;
            done(format!("Set {path}"))
        }
        Operation::ConfDelete { path } => {
            conf()?
                .delete_within(path, CONFD_BOUND)
                .map_err(confd_refusal)?;
            done(format!("Deleted {path}"))
        }
        Operation::ConfGet { path } => {
            let value = conf()?
                .get_value_within(path, CONFD_BOUND)
                .map_err(confd_refusal)?;
            let values = match value {
                Some(value) => {
                    let (kind, text) = value_args(&value);
                    alloc::vec![kind.to_string(), text]
                }
                None => Vec::new(),
            };
            Ok((format!("Read {path}"), values))
        }
        Operation::ConfList { prefix } => {
            let mut paths = conf()?
                .list_within(prefix, CONFD_BOUND)
                .map_err(confd_refusal)?;
            paths.truncate(MAX_KEYS);
            Ok((format!("{} keys", paths.len()), paths))
        }
        Operation::ConfElevate => done(String::from("Settings elevated")),
        Operation::TimeSet { unix } => {
            let client = timed::Client::connect(Native).map_err(Refusal::of_call)?;
            client.set_time(*unix).map_err(Refusal::of_call)?;
            done(String::from("Clock set"))
        }
        Operation::AccountCreate {
            name,
            secret,
            admin,
        } => {
            let user = accounts_client()
                .map_err(Refusal::of_call)?
                .create_within(name, secret, *admin, ACCOUNT_BOUND)
                .map_err(Refusal::of_call)?;
            done(format!("Created {} (uid {})", user.name, user.uid))
        }
        Operation::AccountDelete { name, home } => {
            accounts_client()
                .map_err(Refusal::of_call)?
                .delete_within(name, home.word(), ACCOUNT_BOUND)
                .map_err(Refusal::of_call)?;
            done(format!("Deleted {name}"))
        }
        Operation::AccountAdmin { name, admin } => {
            accounts_client()
                .map_err(Refusal::of_call)?
                .set_admin_within(name, *admin, ACCOUNT_BOUND)
                .map_err(Refusal::of_call)?;
            done(format!("Changed {name}"))
        }
        Operation::AccountPassword { name, secret } => {
            accounts_client()
                .map_err(Refusal::of_call)?
                .set_password_within(name, None, secret, ACCOUNT_BOUND)
                .map_err(Refusal::of_call)?;
            done(format!("Password of {name} set"))
        }
        Operation::PowerPolicy { key, value } => {
            let path = format!("{POWER_PREFIX}{key}");
            conf()?
                .set_value_within(&path, &Value::Str(value.clone()), CONFD_BOUND)
                .map_err(confd_refusal)?;
            done(format!("Set {path}"))
        }
        Operation::NetConfig {
            card,
            address,
            gateway,
            dns,
        } => {
            net_config(card, address, gateway, dns)?;
            done(format!("Configured {card}"))
        }
        Operation::ServiceRestart { name } => {
            let pid = InitClient::try_connect(Native)
                .and_then(|init| init.restart_service_within(name, CONFD_BOUND))
                .map_err(Refusal::of_call)?;
            done(format!("Restarted {name} (was pid {pid})"))
        }
        Operation::WifiSystem(change) => {
            // `keyd` takes a `system` secret from `elevd`'s identity alone,
            // so this call is what the approval bought. The detail names the
            // secret, never what it holds.
            let client = keyd::Client::try_connect(Native)
                .map_err(|_| Refusal::new(errno::ENOENT, "the key service is not running"))?;
            match change {
                WifiChange::Store { name, passphrase } => {
                    client
                        .store_secret("system", name, passphrase.as_bytes())
                        .map_err(Refusal::of_call)?;
                    done(format!("Saved the Wi-Fi password {name}"))
                }
                WifiChange::Delete { name } => {
                    client
                        .delete_secret("system", name)
                        .map_err(Refusal::of_call)?;
                    done(format!("Deleted the Wi-Fi password {name}"))
                }
            }
        }
    }
}

/// `netd` re-reads `sys/net/<card>/*` every few seconds, so the values go in
/// first and `mode` last: it never sees `static` with the old address. This is
/// the order of `xui_app::net::model::plan`.
fn net_config(card: &str, address: &str, gateway: &str, dns: &str) -> Result<(), Refusal> {
    let client = conf()?;
    let key = |name: &str| format!("{NET_PREFIX}{card}/{name}");
    let set = |name: &str, value: &str| {
        client
            .set_value_within(&key(name), &Value::Str(value.to_string()), CONFD_BOUND)
            .map_err(confd_refusal)
    };
    if address.is_empty() {
        return set("mode", "dhcp");
    }
    set("address", address)?;
    for (name, value) in [("gateway", gateway), ("dns", dns)] {
        if !value.is_empty() {
            set(name, value)?;
        } else if let Err(error) = client.delete_within(&key(name), CONFD_BOUND) {
            // Clearing a value that was never set is fine.
            if error != CallError::Service(confd::StoreError::NotFound) {
                return Err(confd_refusal(error));
            }
        }
    }
    set("mode", "static")
}

fn done(detail: String) -> Result<Done, Refusal> {
    Ok((detail, Vec::new()))
}

/// A `confd` client; each call is bounded by [`CONFD_BOUND`].
fn conf() -> Result<confd::Client, Refusal> {
    confd::connect().map_err(|_| Refusal::new(errno::ENOENT, "the settings service is not running"))
}

/// A store failure as the refusal the asker gets.
fn confd_refusal(error: confd::ClientError) -> Refusal {
    Refusal::of_call(confd::flatten(error))
}
