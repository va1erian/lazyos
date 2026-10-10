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
use user::messenger::{accounts, confd, errno, keyd, pkgd, services, timed};
use user::sys;

use super::package::Approved;
use super::{accounts_endpoint, Refusal};

/// How long a configuration call may take (PIT ticks).
const CONFD_TICKS: u64 = 300;
/// How long an account change may take: Argon2id and a home under TCG.
const ACCOUNT_TICKS: u64 = 9000;
/// Most keys `conf.list` returns.
const MAX_KEYS: usize = 512;

/// What the asker gets: a description and the operation's values.
pub(crate) type Done = (String, Vec<String>);

/// Perform `op`. A package install carries what the prompt described
/// (`package.rs`): `pkgd` installs those bytes or nothing.
pub(crate) fn perform(op: &Operation, package: Option<&Approved>) -> Result<Done, Refusal> {
    match op {
        Operation::PkgInstall { .. } | Operation::PkgUpdateCore { .. } => {
            let approved = package.ok_or_else(|| {
                Refusal::new(
                    errno::EINVAL,
                    "the package was not inspected before approval",
                )
            })?;
            let core = matches!(op, Operation::PkgUpdateCore { .. });
            let client = pkgd::Client::connect().map_err(Refusal::of)?;
            let app = client
                .install_approved(&approved.path, &approved.digest, core)
                .map_err(|failure| Refusal(failure.code, failure.text))?;
            done(format!("Installed {} {}", app.system_name, app.version))
        }
        Operation::PkgRemove { system_name } => {
            let client = pkgd::Client::connect().map_err(Refusal::of)?;
            client
                .remove(system_name)
                .map_err(|failure| Refusal(failure.code, failure.text))?;
            done(format!("Removed {system_name}"))
        }
        Operation::ConfSet { path, value } => {
            conf()?.set(path, value).map_err(Refusal::of)?;
            done(format!("Set {path}"))
        }
        Operation::ConfDelete { path } => {
            conf()?.delete(path).map_err(Refusal::of)?;
            done(format!("Deleted {path}"))
        }
        Operation::ConfGet { path } => {
            let value = conf()?.get(path).map_err(Refusal::of)?;
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
            let mut paths = conf()?.list(prefix).map_err(Refusal::of)?;
            paths.truncate(MAX_KEYS);
            Ok((format!("{} keys", paths.len()), paths))
        }
        Operation::ConfElevate => done(String::from("Settings elevated")),
        Operation::TimeSet { unix } => {
            let client = timed::Client::connect().map_err(Refusal::of)?;
            client.set_time(*unix).map_err(Refusal::of)?;
            done(String::from("Clock set"))
        }
        Operation::AccountCreate {
            name,
            secret,
            admin,
        } => {
            let endpoint = accounts_endpoint().map_err(Refusal::of)?;
            let user = accounts::create(&endpoint, name, secret, *admin, deadline())
                .map_err(Refusal::of)?;
            done(format!("Created {} (uid {})", user.name, user.uid))
        }
        Operation::AccountDelete { name, home } => {
            let endpoint = accounts_endpoint().map_err(Refusal::of)?;
            accounts::delete(&endpoint, name, home.word(), deadline()).map_err(Refusal::of)?;
            done(format!("Deleted {name}"))
        }
        Operation::AccountAdmin { name, admin } => {
            let endpoint = accounts_endpoint().map_err(Refusal::of)?;
            accounts::set_admin(&endpoint, name, *admin, deadline()).map_err(Refusal::of)?;
            done(format!("Changed {name}"))
        }
        Operation::AccountPassword { name, secret } => {
            let endpoint = accounts_endpoint().map_err(Refusal::of)?;
            accounts::set_password(&endpoint, name, None, secret, deadline())
                .map_err(Refusal::of)?;
            done(format!("Password of {name} set"))
        }
        Operation::PowerPolicy { key, value } => {
            let path = format!("{POWER_PREFIX}{key}");
            conf()?
                .set(&path, &Value::Str(value.clone()))
                .map_err(Refusal::of)?;
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
            let init = services::resolve_service(services::INIT_NAME).map_err(Refusal::of)?;
            let pid =
                services::init::restart_service(&init, name, Some(sys::clock() + CONFD_TICKS))
                    .map_err(Refusal::of)?;
            done(format!("Restarted {name} (was pid {pid})"))
        }
        Operation::WifiSystem(change) => {
            // `keyd` takes a `system` secret from `elevd`'s identity alone,
            // so this call is what the approval bought. The detail names the
            // secret, never what it holds.
            let client = keyd::Client::connect()
                .map_err(|_| Refusal::new(errno::ENOENT, "the key service is not running"))?;
            match change {
                WifiChange::Store { name, passphrase } => {
                    client
                        .store_secret("system", name, passphrase.as_bytes())
                        .map_err(Refusal::of)?;
                    done(format!("Saved the Wi-Fi password {name}"))
                }
                WifiChange::Delete { name } => {
                    client.delete_secret("system", name).map_err(Refusal::of)?;
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
            .set(&key(name), &Value::Str(value.to_string()))
            .map_err(Refusal::of)
    };
    if address.is_empty() {
        return set("mode", "dhcp");
    }
    set("address", address)?;
    for (name, value) in [("gateway", gateway), ("dns", dns)] {
        if !value.is_empty() {
            set(name, value)?;
        } else if let Err(error) = client.delete(&key(name)) {
            // Clearing a value that was never set is fine.
            if error.errno() != Some(-errno::ENOENT) {
                return Err(Refusal::of(error));
            }
        }
    }
    set("mode", "static")
}

fn done(detail: String) -> Result<Done, Refusal> {
    Ok((detail, Vec::new()))
}

fn deadline() -> Option<u64> {
    Some(sys::clock() + ACCOUNT_TICKS)
}

/// A bounded `confd` client.
fn conf() -> Result<confd::Client, Refusal> {
    confd::Client::connect()
        .map(|client| client.with_timeout(CONFD_TICKS))
        .map_err(|_| Refusal::new(errno::ENOENT, "the settings service is not running"))
}
