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

use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use pkgstore::rules::{self, CompileError};
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

/// Compile `manifest`'s permissions (plus the baseline, `rules::installed`)
/// and load them as the policy of
/// `app:<system_name>`. Returns the number of rules the kernel now holds.
pub(crate) fn load(manifest: &Manifest) -> core::result::Result<u64, LoadError> {
    let rules = rules::installed(manifest).map_err(LoadError::Compile)?;
    policy::load_label(&rules::label(&manifest.app.system_name), &rules).map_err(LoadError::Kernel)
}

/// Withdraw every grant of `system_name`: an empty rule list. The label stays
/// interned (the table is append-only) but a task still running under it is
/// denied everything from this moment on.
pub(crate) fn revoke(system_name: &str) -> Result<u64> {
    let none: Vec<_> = Vec::new();
    policy::load_label(&rules::label(system_name), &none)
}
