//! A package request, before the prompt (review of #659).
//!
//! `pkg.install`, `pkg.update-core` and `pkg.install-devtool` name a file the asker can change at
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
//! What is refused here, before anyone is asked: a file the asker could not
//! install from themselves (`elevd` reads as a system service, so it applies
//! the asker's own source rule, `pkgstore::access::source_allowed`: the
//! shared `/transient`, `/system/share` or the asker's home; otherwise a
//! request would tell the asker about files it cannot read), a package with
//! problems, `pkg.install` of a package that would replace a core app (only
//! `pkg.update-core` may), and `pkg.update-core` of one that would not.
//!
//! A developer-tool package (`docs/devtool-packages-plan.md` section 4) is
//! installed only as `pkg.install-devtool`, whose prompt leads with the
//! class warning (`elevpolicy::package::tool_summary`): the other two refuse
//! it before any prompt, naming the operation, and `pkg.install-devtool`
//! refuses a package that is not one. It may replace a core app only when
//! that core package is itself a developer tool (an update of the Playground
//! from the Installer): a core package never changes class, in either
//! direction, for any of the three operations. `pkgd` makes the same
//! decisions again on `InstallApproved`.

use alloc::string::String;
use alloc::vec::Vec;

use elevpolicy::approvals::Caller;
use elevpolicy::package::{self, Facts, Permission};
use elevpolicy::Operation;
use pkgstore::{access, toolgate};
use user::messenger::accounts::UserRecord as User;
use user::messenger::errno;
use user::messenger::pkgd::{self, wire, PackageInfo};

use super::Refusal;

/// What an approved package request installs.
pub(crate) struct Approved {
    /// The prompt's summary of the package.
    pub(crate) summary: String,
    /// The archive's SHA-256, as `pkgd` reported it.
    pub(crate) digest: String,
    /// The normalised path that was inspected (and is installed).
    pub(crate) path: String,
    /// Whether the install replaces a core app (the `core` flag of
    /// `InstallApproved`): `pkg.update-core`, or `pkg.install-devtool` of an
    /// update to a core developer tool.
    pub(crate) core: bool,
}

/// Inspect the package `op` names for `caller` (whose account is `asker`);
/// `None` when `op` is not a package install.
pub(crate) fn prepare(
    op: &Operation,
    caller: Caller,
    asker: Option<&User>,
) -> Option<Result<Approved, Refusal>> {
    let (path, kind) = match op {
        Operation::PkgInstall { path } => (path, Install::Plain),
        Operation::PkgUpdateCore { path } => (path, Install::UpdateCore),
        Operation::PkgInstallDevtool { path } => (path, Install::Devtool),
        _ => return None,
    };
    Some(source(path, caller, asker).and_then(|path| inspect(&path, kind)))
}

/// Which package operation is being prepared.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Install {
    /// `pkg.install`.
    Plain,
    /// `pkg.update-core`.
    UpdateCore,
    /// `pkg.install-devtool`.
    Devtool,
}

/// Whether `info` (`pkgd`'s inspection) is a developer-tool package: its
/// consent rows open with the class row (`pkgstore::explain::tool`).
pub(crate) fn is_developer_tool(info: &PackageInfo) -> bool {
    info.permissions.iter().any(|permission| {
        permission.kind == pkgstore::explain::tool::KIND_ROW && permission.value == toolgate::KIND
    })
}

/// `path` normalised, if the asker could install from it themselves.
fn source(path: &str, caller: Caller, asker: Option<&User>) -> Result<String, Refusal> {
    let asker_caller = access::Caller {
        uid: caller.uid,
        session: caller.session,
        label_id: caller.label,
        system: false,
    };
    let home = asker.map(|user| user.home.as_str());
    access::source_allowed(&asker_caller, home, path).map_err(|why| Refusal(errno::EPERM, why))
}

fn inspect(path: &str, kind: Install) -> Result<Approved, Refusal> {
    let client = pkgd::Client::connect().map_err(Refusal::of)?;
    let failure = |failure: pkgd::Failure| Refusal(failure.code, failure.text);
    let info = client.inspect(path).map_err(failure)?;
    if let Some(problem) = info.problems.first() {
        return Err(Refusal::new(errno::EINVAL, problem));
    }
    match (kind, is_developer_tool(&info)) {
        (Install::Devtool, false) => {
            return Err(Refusal::new(errno::EINVAL, toolgate::NOT_A_DEVELOPER_TOOL))
        }
        (Install::Plain | Install::UpdateCore, true) => {
            return Err(Refusal::new(
                errno::EPERM,
                toolgate::NEEDS_DEVTOOL_OPERATION,
            ))
        }
        _ => {}
    }
    let installed = client.installed(&info.system_name).map_err(failure)?;
    let core = installed
        .as_ref()
        .filter(|row| row.origin == wire::ORIGIN_CORE);
    let tool = is_developer_tool(&info);
    // A core package keeps its class: an administrator updates a core
    // developer tool with `pkg.install-devtool`, but neither turns a core
    // app into a tool nor a tool into an app (`pkgd` decides again).
    if let Some(row) = core {
        toolgate::core_class_change(toolgate::is_tool_kind(&row.kind), tool)
            .map_err(|why| Refusal::new(errno::EPERM, why))?;
    }
    // `pkg.update-core` names the replacement of a core app; so does
    // `pkg.install-devtool` when the core package it replaces is a tool.
    let update = kind == Install::UpdateCore || (kind == Install::Devtool && core.is_some());
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
    let replaced = replaces.is_some();
    let facts = facts(&info, replaces);
    let summary = match kind {
        Install::Devtool => package::tool_summary(&facts),
        _ => package::summary(&facts),
    };
    Ok(Approved {
        summary,
        digest: info.digest,
        path: String::from(path),
        core: replaced,
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
