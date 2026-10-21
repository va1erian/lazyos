//! Loading and revoking an installed app's kernel policy.
//!
//! What a manifest's permissions *mean* is `pkgstore::rules` (a pure function,
//! tested on the host); this module hands the result to the kernel. The
//! `acl_load` operation needs `CAP_IPC_CONTROL`: `pkgd` is spawned by `init`
//! with `init`'s own identity (root, every capability but the raw-input one),
//! and that is deliberate, because the package manager is the one task that
//! decides what an app may do. `load_label` replaces *every* rule of the label
//! at once, so a policy swap on upgrade is atomic and revoking is loading an
//! empty list.
//!
//! A developer tool (`[app] kind = "developer-tool"`) is loaded the same way
//! under its own label, `tool:<system_name>` (`rules::label_of`), with the
//! tool rules `rules::installed` compiles for it; every revoke names the same
//! label, from the manifest or from the row's recorded kind.
//!
//! An app that declares `[processes.<name>]` tables also gets one child label
//! each (`app:<system_name>/<name>`, issue #832): loaded after the app's own
//! label, which the kernel requires, and with the program that enters it. The
//! kernel refuses a child that holds more than the app, and revoking the app
//! revokes its children, so `pkgd` only has to name the ones an upgrade drops.

use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use pkgstore::rules::{self, CompileError};
use user::messenger::pkgd::Installed;
use user::messenger::policy;
use user::messenger::{Error, Result};

/// Why a policy could not be loaded.
pub(crate) enum LoadError {
    /// The manifest asks for more rules than the kernel stores per app.
    Compile(CompileError),
    /// The kernel refused the load.
    Kernel(Error),
}

impl LoadError {
    /// Friendly text for the failure.
    pub(crate) fn text(&self) -> String {
        match self {
            LoadError::Compile(error) => alloc::format!("{error}"),
            LoadError::Kernel(error) => alloc::format!(
                "the system would not accept the application's permissions: {}",
                error.message()
            ),
        }
    }
}

/// What the kernel holds after a [`load`].
pub(crate) struct Loaded {
    /// The rules of the app's own label.
    pub(crate) rules: u64,
    /// Each child label with the number of its rules and the program that
    /// enters it.
    pub(crate) children: Vec<(String, u64, String)>,
}

/// Compile `manifest`'s permissions (plus the baseline, `rules::installed`)
/// and load them as the policy of its label (`rules::label_of`:
/// `app:<system_name>`, or `tool:<system_name>` for a developer tool), then each of its
/// declared child labels. Everything is compiled before anything is loaded;
/// if a child is refused, the app's own label is revoked again too (which
/// takes the children with it): an app whose helpers could not be confined
/// is never left loaded, or the helper would run with the app's own label.
pub(crate) fn load(manifest: &Manifest) -> core::result::Result<Loaded, LoadError> {
    let app_rules = rules::installed(manifest).map_err(LoadError::Compile)?;
    let children = rules::children(manifest).map_err(LoadError::Compile)?;
    let app =
        policy::load_label(&rules::label_of(manifest), &app_rules).map_err(LoadError::Kernel)?;
    let mut loaded = Loaded {
        rules: app,
        children: Vec::with_capacity(children.len()),
    };
    for child in &children {
        match policy::load_child_label(&child.label, &child.rules, &child.program) {
            Ok(count) => loaded
                .children
                .push((child.label.clone(), count, child.program.clone())),
            Err(error) => {
                // Fail closed: the kernel drops the children with the app.
                let _ = revoke(manifest);
                return Err(LoadError::Kernel(error));
            }
        }
    }
    Ok(loaded)
}

/// Withdraw every grant of the app `manifest` describes: an empty rule list
/// for its label. The label stays interned (the table is append-only) but a
/// task still running under it is denied everything from this moment on. The
/// kernel revokes the app's child labels with it.
pub(crate) fn revoke(manifest: &Manifest) -> Result<u64> {
    revoke_label(&rules::label_of(manifest))
}

/// [`revoke`] for an installed row, whose manifest may be unreadable: the
/// label its recorded kind names.
pub(crate) fn revoke_row(row: &Installed) -> Result<u64> {
    revoke_label(&rules::label_for_kind(&row.system_name, &row.kind))
}

/// Revoke one label by name: an app label, or a child label (no rules, no
/// program: the kernel drops its binding).
pub(crate) fn revoke_label(label: &str) -> Result<u64> {
    let none: Vec<_> = Vec::new();
    policy::load_label(label, &none)
}

/// Revoke the child labels `old` declared that `new` does not (every one when
/// `new` is `None`, the app being removed). Returns how many were revoked.
pub(crate) fn revoke_dropped(old: &Manifest, new: Option<&Manifest>) -> usize {
    rules::dropped(old, new)
        .iter()
        .filter(|label| revoke_label(label).is_ok())
        .count()
}
