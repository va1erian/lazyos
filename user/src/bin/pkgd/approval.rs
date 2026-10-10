//! What an install may do to a core app, and whether it is what an
//! administrator approved (docs/accounts-plan.md U2, review of #659).
//!
//! Replacing a core app is a system change. `Install` never does it for a
//! session or for `elevd`; `elevd` installs through `InstallApproved`,
//! naming what the trusted prompt showed: the archive's SHA-256 (so the
//! file cannot be swapped after the approval) and whether a core app is
//! replaced (`pkg.update-core`) or must not be (`pkg.install`).

use alloc::format;

use user::messenger::pkgd::{Failure, PackageInfo};

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
    /// when `core` (`pkg.update-core`).
    Approved { digest: &'a str, core: bool },
    /// `InstallDebug` from `dbgd` on a box built for remote control
    /// (docs/dbgd-plan.md, v2): only the bytes whose SHA-256 is `digest`,
    /// core app or not (a developer iterates on core apps too).
    Debug { digest: &'a str },
}

impl Pkgd {
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
            Approval::Approved { digest, core } => {
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
        if !core && matches!(approval, Approval::Approved { core: true, .. }) {
            return Err(fail(
                EINVAL,
                format!(
                    "{} is not a core app: install it with pkg.install",
                    info.name
                ),
            ));
        }
        Ok(())
    }
}
