//! What an install may do to a core app, and whether it is what an
//! administrator approved (docs/accounts-plan.md U2, review of #659).
//!
//! Replacing a core app is a system change. `Install` never does it for a
//! session or for `elevd`; `elevd` installs through `InstallApproved`,
//! naming what the trusted prompt showed: the archive's SHA-256 (so the
//! file cannot be swapped after the approval) and whether a core app is
//! replaced (`pkg.update-core`) or must not be (`pkg.install`), and whether
//! the administrator saw the developer-tool warning (`pkg.install-devtool`,
//! `pkgstore::toolgate`). Both flags together update a core developer tool;
//! a core package never changes class ([`Pkgd::core_keeps_class`]).

use alloc::format;

use pkgstore::toolgate::{self, Route};
use user::messenger::pkgd::{Failure, Installed, PackageInfo};

use super::handlers::{fail, Pkgd, EINVAL, EPERM};

/// What an install may do to a core app (docs/accounts-plan.md U2, review
/// of #659).
#[derive(Clone, Copy)]
pub(crate) enum Approval<'a> {
    /// `Install`: a core app is replaced only for a system service that
    /// holds `CAP_SETUID`; never for a session, never for `elevd`.
    Plain { replace_core: bool },
    /// `InstallApproved` from `elevd`: only the bytes whose SHA-256 is
    /// `digest` (what the prompt described), replacing a core app exactly
    /// when `core` (`pkg.update-core`), and a developer-tool package exactly
    /// when `devtool` (`pkg.install-devtool`).
    Approved {
        digest: &'a str,
        core: bool,
        devtool: bool,
    },
    /// `InstallDebug` from `dbgd` on a box built for remote control
    /// (docs/dbgd-plan.md, v2): only the bytes whose SHA-256 is `digest`,
    /// core app or not (a developer iterates on core apps too).
    Debug { digest: &'a str },
}

impl Approval<'_> {
    /// The door the install came through, for `pkgstore::toolgate::install`.
    pub(crate) fn route(&self) -> Route {
        match self {
            Approval::Plain { .. } => Route::Plain,
            Approval::Approved { devtool, .. } => Route::Approved { devtool: *devtool },
            Approval::Debug { .. } => Route::Debug,
        }
    }
}

impl Pkgd {
    /// Whether an approved install of the package `info` (`tool`: a
    /// developer tool) over `previous`, the row now recorded, would change
    /// the class of a core package, or its shell role (`shell`: the new
    /// manifest's `shell = true`). The administrator saw one prompt for one
    /// class (`pkg.install-devtool` leads with the tool warning,
    /// `pkg.update-core` does not), so a core package keeps its class and
    /// the prompt's "shell tool" (or its absence) stays true.
    pub(crate) fn core_keeps_class(
        &self,
        info: &PackageInfo,
        tool: bool,
        shell: bool,
        previous: Option<&Installed>,
        approval: Approval<'_>,
    ) -> Result<(), Failure> {
        let installed_core = self.core_version(&info.system_name).is_some();
        if !installed_core || !matches!(approval, Approval::Approved { .. }) {
            return Ok(());
        }
        let was_tool = previous.is_some_and(|row| toolgate::is_tool_kind(&row.kind));
        let was_shell = previous.is_some_and(|row| row.shell);
        toolgate::core_class_change(was_tool, tool, was_shell, shell)
            .map_err(|why| fail(EPERM, why))
    }

    /// Whether `approval` covers installing the package `info` describes:
    /// the approved bytes, and a core app replaced exactly when allowed.
    pub(crate) fn approved(
        &self,
        info: &PackageInfo,
        approval: Approval<'_>,
    ) -> Result<(), Failure> {
        let core = self.core_version(&info.system_name).is_some();
        let replace_core = match approval {
            Approval::Plain { replace_core } => replace_core,
            Approval::Approved { digest, core, .. } => {
                if info.digest != digest {
                    return Err(fail(
                        EINVAL,
                        "the package changed after an administrator approved it",
                    ));
                }
                core
            }
            Approval::Debug { digest } => {
                if info.digest != digest {
                    return Err(fail(
                        EINVAL,
                        "the package is not the one the client uploaded (sha256 differs)",
                    ));
                }
                true
            }
        };
        if core && !replace_core {
            return Err(fail(
                EPERM,
                format!(
                    "{} is a core app: replacing it needs an administrator (elevd pkg.update-core)",
                    info.name
                ),
            ));
        }
        if let (
            false,
            Approval::Approved {
                core: true,
                devtool,
                ..
            },
        ) = (core, approval)
        {
            // Name the operation an administrator would have to approve: a
            // developer tool is never installed with `pkg.install`.
            let operation = if devtool {
                "pkg.install-devtool"
            } else {
                "pkg.install"
            };
            return Err(fail(
                EINVAL,
                format!(
                    "{} is not a core app: install it with {operation}",
                    info.name
                ),
            ));
        }
        Ok(())
    }
}
