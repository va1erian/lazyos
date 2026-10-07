//! What an operation would do, as the trusted prompt and the audit record
//! say it (review of #659).
//!
//! A summary is the administrator's whole view of the change, so it never
//! hides its tail: every value an asker chose is escaped ([`text::shown`]),
//! a long path is shortened in the middle (`sys/ui/.../demo`), a long text
//! value keeps its head and tail and names its length, and the whole stays
//! within [`MAX_SUMMARY`] characters, which the prompt shows in full
//! (`xuid`'s `prompt_draw.rs`). A package's summary is
//! [`crate::package::summary`], from `pkgd`'s inspection.

use alloc::format;
use alloc::string::String;

use crate::text::{self, elide, elide_path};
use crate::values::{civil, value_args};
use crate::{HomeFate, Operation, Value};

/// Longest summary (characters); the prompt has room for all of it.
pub const MAX_SUMMARY: usize = 300;
/// Longest path a summary shows.
const PATH_SHOWN: usize = 64;
/// Longest text value a summary shows (quotes and length note aside).
const VALUE_SHOWN: usize = 96;

impl Operation {
    /// What the operation would do, for the prompt and the audit record. It
    /// never carries a password, and holds only printable ASCII and Latin-1.
    pub fn summary(&self) -> String {
        match self {
            Operation::PkgInstall { path } => {
                format!("Install the package {} for all users", path_text(path))
            }
            Operation::PkgUpdateCore { path } => {
                format!("Replace a core app with {}", path_text(path))
            }
            Operation::PkgRemove { system_name } => {
                format!("Remove the app {}", elide(system_name, PATH_SHOWN))
            }
            Operation::ConfSet { path, value } => {
                format!(
                    "Set the setting {} to {}",
                    elide_path(path, PATH_SHOWN),
                    value_text(value)
                )
            }
            Operation::ConfDelete { path } => {
                format!("Delete the setting {}", elide_path(path, PATH_SHOWN))
            }
            Operation::ConfList { .. } | Operation::ConfGet { .. } | Operation::ConfElevate => {
                String::from("Read and change every setting, system settings included")
            }
            Operation::TimeSet { unix } => format!("Set the clock to {}", civil(*unix)),
            Operation::AccountCreate { name, admin, .. } => {
                let kind = if *admin { "an administrator" } else { "a user" };
                format!("Create the account '{name}' ({kind})")
            }
            Operation::AccountDelete { name, home } => {
                let home = match home {
                    HomeFate::Keep => "its home is kept",
                    HomeFate::Archive => "its home is archived",
                    HomeFate::Remove => "its home is removed",
                };
                format!("Delete the account '{name}' ({home})")
            }
            Operation::AccountAdmin { name, admin: true } => {
                format!("Make '{name}' an administrator")
            }
            Operation::AccountAdmin { name, admin: false } => {
                format!("Take administrator rights from '{name}'")
            }
            Operation::AccountPassword { name, .. } => format!("Set the password of '{name}'"),
            Operation::PowerPolicy { key, value } => {
                format!(
                    "Set the power policy {key} to {}",
                    text::quoted(value, VALUE_SHOWN)
                )
            }
            Operation::ServiceRestart { name } => format!("Restart the system service '{name}'"),
        }
    }
}

/// A package file path: shortened in the middle, its file name kept.
fn path_text(path: &str) -> String {
    format!(
        "/{}",
        elide_path(path.trim_start_matches('/'), PATH_SHOWN - 1)
    )
}

/// A value as the prompt shows it: text quoted and escaped, bytes as a
/// length only.
fn value_text(value: &Value) -> String {
    match value {
        Value::Str(text) => text::quoted(text, VALUE_SHOWN),
        Value::Bytes(bytes) => format!("{} bytes", bytes.len()),
        other => value_args(other).1,
    }
}
