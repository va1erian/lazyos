//! Carrying out an approved operation, as `elevd`.
//!
//! Each service accepts these calls from `elevd`'s identity alone
//! (`elevpolicy::is_elevd`), so nothing here hands privilege to the asker:
//! the asker gets the result, never a capability. The call is bounded, and
//! the service's own refusal comes back to the asker as it was given.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use elevpolicy::{value_args, Operation, Value, POWER_PREFIX};
use user::messenger::{accounts, confd, errno, pkgd, services, timed};
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
        Operation::PkgInstall { path } | Operation::PkgUpdateCore { path } => {
            let approved = package.ok_or_else(|| {
                Refusal::new(
                    errno::EINVAL,
                    "the package was not inspected before approval",
                )
            })?;
            let core = matches!(op, Operation::PkgUpdateCore { .. });
            let client = pkgd::Client::connect().map_err(Refusal::of)?;
            let app = client
                .install_approved(path, &approved.digest, core)
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
        Operation::ServiceRestart { name } => {
            let init = services::resolve_service(services::INIT_NAME).map_err(Refusal::of)?;
            let pid =
                services::init::restart_service(&init, name, Some(sys::clock() + CONFD_TICKS))
                    .map_err(Refusal::of)?;
            done(format!("Restarted {name} (was pid {pid})"))
        }
    }
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
