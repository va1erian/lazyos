//! A package request, before the prompt (review of #659).
//!
//! `pkg.install` and `pkg.update-core` name a file the asker can change at
//! any time, but the administrator approves a *package*. So before the
//! prompt `elevd` has `pkgd` inspect the file (as a system service, the same
//! validation an install runs) and asks which app it would replace; the
//! prompt then names the package, the core app it replaces and from which
//! version, its author (unverified) and its permissions by risk
//! (`elevpolicy::package`). The install that follows is `InstallApproved`
//! with the archive's SHA-256 from that inspection, so `pkgd` installs those
//! bytes or nothing: a file swapped after the approval is refused. A content
//! hash rather than a private copy: `pkgd` already reads and hashes the
//! whole file into one buffer before it installs from that buffer, so the
//! check and the use are the same bytes, and `elevd` needs no storage of
//! its own.
//!
//! What is refused here, before anyone is asked: a package with problems,
//! `pkg.install` of a package that would replace a core app (only
//! `pkg.update-core` may), and `pkg.update-core` of one that would not.

use alloc::string::String;
use alloc::vec::Vec;

use elevpolicy::package::{self, Facts, Permission};
use elevpolicy::Operation;
use user::messenger::errno;
use user::messenger::pkgd::{self, wire, PackageInfo};

use super::Refusal;

/// What an approved package request installs.
pub(crate) struct Approved {
    /// The prompt's summary of the package.
    pub(crate) summary: String,
    /// The archive's SHA-256, as `pkgd` reported it.
    pub(crate) digest: String,
}

/// Inspect the package `op` names; `None` when `op` is not a package
/// install.
pub(crate) fn prepare(op: &Operation) -> Option<Result<Approved, Refusal>> {
    let (path, update) = match op {
        Operation::PkgInstall { path } => (path, false),
        Operation::PkgUpdateCore { path } => (path, true),
        _ => return None,
    };
    Some(inspect(path, update))
}

fn inspect(path: &str, update: bool) -> Result<Approved, Refusal> {
    let client = pkgd::Client::connect().map_err(Refusal::of)?;
    let failure = |failure: pkgd::Failure| Refusal(failure.code, failure.text);
    let info = client.inspect(path).map_err(failure)?;
    if let Some(problem) = info.problems.first() {
        return Err(Refusal::new(errno::EINVAL, problem));
    }
    let installed = client.installed(&info.system_name).map_err(failure)?;
    let core = installed
        .as_ref()
        .filter(|row| row.origin == wire::ORIGIN_CORE);
    let replaces = match (core, update) {
        (Some(row), true) => Some(row.version.clone()),
        (None, false) => None,
        (Some(_), false) => {
            return Err(Refusal::new(
                errno::EPERM,
                "that package would replace a core app: only pkg.update-core may",
            ))
        }
        (None, true) => {
            return Err(Refusal::new(
                errno::EINVAL,
                "that package does not replace a core app: install it with pkg.install",
            ))
        }
    };
    Ok(Approved {
        summary: package::summary(&facts(&info, replaces)),
        digest: info.digest,
    })
}

/// The prompt's view of `pkgd`'s inspection.
fn facts(info: &PackageInfo, replaces: Option<String>) -> Facts {
    let permissions: Vec<Permission> = info
        .permissions
        .iter()
        .map(|p| Permission {
            kind: p.kind.clone(),
            value: p.value.clone(),
            risk: p.risk.clone(),
        })
        .collect();
    Facts {
        name: info.name.clone(),
        system_name: info.system_name.clone(),
        version: info.version.clone(),
        author: info.author.clone(),
        permissions,
        replaces,
    }
}
