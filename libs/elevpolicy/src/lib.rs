//! `elevd`'s operation table (docs/accounts-plan.md U2, issue #625).
//!
//! Nobody is ever handed root or a capability. A program that needs a
//! privileged change asks `elevd` for one **operation** of the fixed table
//! below, by name and with string arguments; `elevd` checks them here, shows
//! the [`Operation::summary`] on the trusted prompt, and performs the
//! operation itself once an administrator typed their password. The
//! services that carry out the operations accept them from `elevd`'s
//! identity alone ([`is_elevd`]).
//!
//! | operation | arguments | performed by |
//! |---|---|---|
//! | `pkg.install` | path | `pkgd` `InstallApproved`, never over a core app |
//! | `pkg.update-core` | path | `pkgd` `InstallApproved` replacing a core app |
//! | `pkg.remove` | system name | `pkgd` `Remove` |
//! | `conf.set` | path, kind, value | `confd` `Set` (`sys/**`, any user's keys) |
//! | `conf.delete` | path | `confd` `Delete` |
//! | `conf.list` | prefix | `confd` `List`, every key |
//! | `conf.get` | path | `confd` `Get`, any key |
//! | `conf.elevate` | none | (approval only: the elevated editor) |
//! | `time.set` | UTC seconds | `timed` `SetTime` |
//! | `account.create` | name, password, `admin`/`user` | `accountsd` `Create` |
//! | `account.delete` | name, `keep`/`archive`/`remove` | `accountsd` `Delete` |
//! | `account.admin` | name, `1`/`0` | `accountsd` `SetAdmin` |
//! | `account.password` | name, password | `accountsd` `SetPassword` |
//! | `power.policy` | key, value | `confd` `Set` of `sys/power/<key>` |
//! | `service.restart` | service name ([`RESTARTABLE`]) | `init` `RestartService` |
//!
//! Every change asks every time: each `conf.set`, `conf.delete` and every
//! other operation opens the prompt (decision of 2026-10-07). Only the
//! elevated editor's *view* stands: once `conf.elevate` is approved, the same
//! caller (uid, label and session, [`approvals`]) may read every key
//! (`conf.list`, `conf.get`) for [`APPROVAL_TICKS`] without a prompt per row.
//! A read changes nothing, and every user's private keys stay behind that one
//! approval.
//!
//! The prompt is never free to raise: [`backoff`] holds back a caller whose
//! prompts were cancelled or timed out, [`queue`] gives each caller one
//! request in hand at a time, and [`sessions`] ends what a session held when
//! `logind` reports it over.
//!
//! What the prompt and the audit trail say is built here too, so it can be
//! tested on the host: [`Operation::summary`] and [`package::summary`] are
//! escaped and bounded to [`MAX_SUMMARY`] ([`text`]), and [`audit`] writes
//! lines no value can forge. A request [`Operation::parse`] accepts can
//! still be refused by policy before any prompt ([`Operation::permitted`]).

#![no_std]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub use accountdb::{ACCOUNTS_UID, ELEVD_UID};
pub use allow::{restartable, RESTARTABLE};
pub use confd::Value;
pub use summary::MAX_SUMMARY;
use values::*;
pub use values::{parse_value, value_args};

mod allow;
pub mod approvals;
pub mod audit;
pub mod backoff;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod package;
pub mod queue;
pub mod sessions;
mod summary;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_flood;
pub mod text;
#[cfg(test)]
mod text_tests;
mod values;

/// How long the elevated editor's view stands (PIT ticks, 100 Hz): 5
/// minutes. Changes never stand ([`Class::Once`]).
pub const APPROVAL_TICKS: u64 = 5 * 60 * 100;
/// Wrong passwords one request may type before it is refused.
pub const PROMPT_ATTEMPTS: u32 = 3;
/// Most arguments an operation takes.
pub const MAX_ARGS: usize = 4;
/// Longest argument accepted.
pub const MAX_ARG: usize = 1024;
/// Longest password accepted (what `keyd` takes).
pub const MAX_SECRET: usize = 64;
/// The confd subtree `power.policy` writes.
pub const POWER_PREFIX: &str = "sys/power/";
/// Latest clock `time.set` accepts (2200-01-01, as `timed`).
const MAX_TIME: i64 = 7_258_118_400;

/// Whether a task with these kernel-stamped credentials is `elevd`: its
/// system uid, unlabelled, outside any session. The one identity the
/// services take privileged requests from.
pub const fn is_elevd(uid: u32, label_id: u32, session: u64) -> bool {
    uid == ELEVD_UID && label_id == 0 && session == 0
}

/// What happens to a deleted account's home.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeFate {
    Keep,
    Archive,
    Remove,
}

impl HomeFate {
    /// The word `accountsd`'s `Delete` takes.
    pub const fn word(self) -> &'static str {
        match self {
            HomeFate::Keep => "keep",
            HomeFate::Archive => "archive",
            HomeFate::Remove => "remove",
        }
    }
}

/// One operation of the table, its arguments checked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Operation {
    PkgInstall {
        path: String,
    },
    PkgUpdateCore {
        path: String,
    },
    PkgRemove {
        system_name: String,
    },
    ConfSet {
        path: String,
        value: Value,
    },
    ConfDelete {
        path: String,
    },
    ConfList {
        prefix: String,
    },
    ConfGet {
        path: String,
    },
    ConfElevate,
    TimeSet {
        unix: i64,
    },
    AccountCreate {
        name: String,
        secret: String,
        admin: bool,
    },
    AccountDelete {
        name: String,
        home: HomeFate,
    },
    AccountAdmin {
        name: String,
        admin: bool,
    },
    AccountPassword {
        name: String,
        secret: String,
    },
    PowerPolicy {
        key: String,
        value: String,
    },
    ServiceRestart {
        name: String,
    },
}

/// Whether an approval of the operation may stand for later requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Reading every key in the elevated editor (`conf.elevate`,
    /// `conf.list`, `conf.get`): approved for [`APPROVAL_TICKS`].
    View,
    /// Asked for every time.
    Once,
}

/// Every operation name, in table order.
pub const NAMES: &[&str] = &[
    "pkg.install",
    "pkg.update-core",
    "pkg.remove",
    "conf.set",
    "conf.delete",
    "conf.list",
    "conf.get",
    "conf.elevate",
    "time.set",
    "account.create",
    "account.delete",
    "account.admin",
    "account.password",
    "power.policy",
    "service.restart",
];

impl Operation {
    /// Check `operation` and its `args`; `Err` says what is wrong.
    pub fn parse(operation: &str, args: &[String]) -> Result<Operation, &'static str> {
        if args.len() > MAX_ARGS || args.iter().any(|arg| arg.len() > MAX_ARG) {
            return Err("too many or too long arguments");
        }
        let arg = |index: usize| args.get(index).map(String::as_str);
        let want = |count: usize| {
            if args.len() == count {
                Ok(())
            } else {
                Err("wrong number of arguments")
            }
        };
        let op = match operation {
            "pkg.install" | "pkg.update-core" => {
                want(1)?;
                let path = package_path(arg(0).unwrap_or(""))?;
                if operation == "pkg.install" {
                    Operation::PkgInstall { path }
                } else {
                    Operation::PkgUpdateCore { path }
                }
            }
            "pkg.remove" => {
                want(1)?;
                let name = arg(0).unwrap_or("");
                if !system_name(name) {
                    return Err("not an app's system name");
                }
                Operation::PkgRemove {
                    system_name: name.to_string(),
                }
            }
            "conf.set" => {
                want(3)?;
                let path = conf_path(arg(0).unwrap_or(""))?;
                let value = parse_value(arg(1).unwrap_or(""), arg(2).unwrap_or(""))?;
                // A text value is shown and stored as it is: nothing in it
                // may make it read other than it is (`text::misleading`).
                if matches!(&value, Value::Str(text) if !text::plain(text)) {
                    return Err("a text value may not hold control or formatting characters");
                }
                Operation::ConfSet { path, value }
            }
            "conf.delete" => {
                want(1)?;
                Operation::ConfDelete {
                    path: conf_path(arg(0).unwrap_or(""))?,
                }
            }
            "conf.get" => {
                want(1)?;
                Operation::ConfGet {
                    path: conf_path(arg(0).unwrap_or(""))?,
                }
            }
            "conf.list" => {
                want(1)?;
                let prefix = arg(0).unwrap_or("");
                if prefix.len() > 256 || !text::plain(prefix) {
                    return Err("not a confd prefix");
                }
                Operation::ConfList {
                    prefix: prefix.to_string(),
                }
            }
            "conf.elevate" => {
                want(0)?;
                Operation::ConfElevate
            }
            "time.set" => {
                want(1)?;
                let unix: i64 = arg(0)
                    .and_then(|text| text.parse().ok())
                    .filter(|unix| (0..MAX_TIME).contains(unix))
                    .ok_or("not a time between 1970 and 2200")?;
                Operation::TimeSet { unix }
            }
            "account.create" => {
                want(3)?;
                let admin = match arg(2) {
                    Some("admin") => true,
                    Some("user") => false,
                    _ => return Err("the kind is admin or user"),
                };
                Operation::AccountCreate {
                    name: account(arg(0).unwrap_or(""))?,
                    secret: secret(arg(1).unwrap_or(""))?,
                    admin,
                }
            }
            "account.delete" => {
                want(2)?;
                let home = match arg(1) {
                    Some("keep") => HomeFate::Keep,
                    Some("archive") => HomeFate::Archive,
                    Some("remove") => HomeFate::Remove,
                    _ => return Err("the home is kept, archived or removed"),
                };
                Operation::AccountDelete {
                    name: account(arg(0).unwrap_or(""))?,
                    home,
                }
            }
            "account.admin" => {
                want(2)?;
                let admin = match arg(1) {
                    Some("1") => true,
                    Some("0") => false,
                    _ => return Err("admin is 1 or 0"),
                };
                Operation::AccountAdmin {
                    name: account(arg(0).unwrap_or(""))?,
                    admin,
                }
            }
            "account.password" => {
                want(2)?;
                Operation::AccountPassword {
                    name: account(arg(0).unwrap_or(""))?,
                    secret: secret(arg(1).unwrap_or(""))?,
                }
            }
            "power.policy" => {
                want(2)?;
                let key = arg(0).unwrap_or("");
                let value = arg(1).unwrap_or("");
                if !word(key) {
                    return Err("not a power policy key");
                }
                if value.is_empty() || value.len() > 64 || !text::plain(value) {
                    return Err("not a power policy value");
                }
                Operation::PowerPolicy {
                    key: key.to_string(),
                    value: value.to_string(),
                }
            }
            "service.restart" => {
                want(1)?;
                let name = arg(0).unwrap_or("");
                if !word(name) {
                    return Err("not a service name");
                }
                Operation::ServiceRestart {
                    name: name.to_string(),
                }
            }
            _ => return Err("not an operation elevd performs"),
        };
        Ok(op)
    }

    /// The operation's table name.
    pub fn name(&self) -> &'static str {
        NAMES[self.index()]
    }

    fn index(&self) -> usize {
        match self {
            Operation::PkgInstall { .. } => 0,
            Operation::PkgUpdateCore { .. } => 1,
            Operation::PkgRemove { .. } => 2,
            Operation::ConfSet { .. } => 3,
            Operation::ConfDelete { .. } => 4,
            Operation::ConfList { .. } => 5,
            Operation::ConfGet { .. } => 6,
            Operation::ConfElevate => 7,
            Operation::TimeSet { .. } => 8,
            Operation::AccountCreate { .. } => 9,
            Operation::AccountDelete { .. } => 10,
            Operation::AccountAdmin { .. } => 11,
            Operation::AccountPassword { .. } => 12,
            Operation::PowerPolicy { .. } => 13,
            Operation::ServiceRestart { .. } => 14,
        }
    }

    /// Whether its approval may stand ([`Class`]).
    pub fn class(&self) -> Class {
        match self {
            Operation::ConfList { .. } | Operation::ConfGet { .. } | Operation::ConfElevate => {
                Class::View
            }
            _ => Class::Once,
        }
    }

    /// The arguments [`Operation::parse`] reads back.
    pub fn args(&self) -> Vec<String> {
        let one = |text: &str| alloc::vec![text.to_string()];
        match self {
            Operation::PkgInstall { path } | Operation::PkgUpdateCore { path } => one(path),
            Operation::PkgRemove { system_name } => one(system_name),
            Operation::ConfSet { path, value } => {
                let (kind, text) = value_args(value);
                alloc::vec![path.clone(), kind.to_string(), text]
            }
            Operation::ConfDelete { path } | Operation::ConfGet { path } => one(path),
            Operation::ConfList { prefix } => one(prefix),
            Operation::ConfElevate => Vec::new(),
            Operation::TimeSet { unix } => alloc::vec![format!("{unix}")],
            Operation::AccountCreate {
                name,
                secret,
                admin,
            } => alloc::vec![
                name.clone(),
                secret.clone(),
                String::from(if *admin { "admin" } else { "user" })
            ],
            Operation::AccountDelete { name, home } => {
                alloc::vec![name.clone(), home.word().to_string()]
            }
            Operation::AccountAdmin { name, admin } => {
                alloc::vec![name.clone(), String::from(if *admin { "1" } else { "0" })]
            }
            Operation::AccountPassword { name, secret } => {
                alloc::vec![name.clone(), secret.clone()]
            }
            Operation::PowerPolicy { key, value } => alloc::vec![key.clone(), value.clone()],
            Operation::ServiceRestart { name } => one(name),
        }
    }
}
