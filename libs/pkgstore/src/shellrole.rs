//! The shell role of a developer tool (`docs/devtool-packages-plan.md`,
//! "T-later: Terminal").
//!
//! A developer tool is a labelled caller, and `pkgd`, `init` and the other
//! services refuse a labelled caller outright, because an app that could
//! install apps or stop the machine would defeat its own sandbox. The
//! Terminal is the user's own shell: what runs under its label (the shell and
//! every command typed in it) must act as the session's owner, as it did when
//! it was an unlabelled program. A package says so with `[app] shell = true`
//! (`lazypkg::Manifest::is_shell`), and the role is kept narrow:
//!
//! * **Only a package the image ships.** A user's or an administrator's own
//!   tool cannot claim it ([`shell_install`]); `pkgd` records the flag in its
//!   row only for a core package ([`shell_of`]).
//! * **Read from the row, never from the package.** Every service decides
//!   with [`row_is_shell`] on the row `pkgd` recorded, which re-checks the
//!   tool class and the core origin, so a manifest edited after the install,
//!   or a row written by any other route, is no shell.
//! * **Session owner, not service.** [`as_session_owner`] drops the label and
//!   never yields a system caller. It does not make a shell an administrator,
//!   a service, or a developer-tool installer, and no other labelled caller
//!   is touched by it.

use crate::access::Caller;
use crate::toolgate::is_tool_kind;

/// The refusal of a package that asks to be the user's shell (`[app] shell =
/// true`) when the image does not ship it.
pub const SHELL_CORE_ONLY: &str =
    "only a developer tool the image ships can be the user's shell (shell = true)";

/// Whether an install of a package that `wants_shell` may go ahead: `core` is
/// whether the image ships a package of that `system_name` (`Pkgd::is_core`).
/// The shell role makes what runs under the tool's label act as the session's
/// owner, so it is never granted by a manifest alone: a core shell can only be
/// replaced by an administrator-approved update of the same package.
pub fn shell_install(wants_shell: bool, core: bool) -> Result<(), &'static str> {
    if wants_shell && !core {
        Err(SHELL_CORE_ONLY)
    } else {
        Ok(())
    }
}

/// Whether a row records a shell tool: the `shell` flag, on a developer tool,
/// shipped by the image. `kind` and `core` are re-checked here on purpose:
/// `pkgd` and `init` read the row on every decision and never trust the flag
/// alone.
pub fn row_is_shell(kind: &str, shell: bool, core: bool) -> bool {
    shell && core && is_tool_kind(kind)
}

/// The `Installed::shell` a manifest records: it asks (`shell = true` on a
/// developer tool) and the image ships the package.
pub fn shell_of(manifest: &lazypkg::Manifest, core: bool) -> bool {
    manifest.is_shell() && core
}

/// `caller` as `pkgd` judges it when its label is a shell tool's (`is_shell`,
/// [`row_is_shell`] on the row of the label's package): the user's own shell
/// acts as the session's owner, so the label is dropped, and it is never a
/// system caller whatever the stamp says (a shell task holds no `CAP_SETUID`;
/// a stamp that claimed one would be a bug, not a privilege). Every other
/// caller is returned unchanged and stays refused where the labelled are.
pub fn as_session_owner(caller: Caller, is_shell: bool) -> Caller {
    if is_shell && caller.label_id != 0 {
        Caller {
            label_id: 0,
            system: false,
            ..caller
        }
    } else {
        caller
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{may_inspect, may_install_developer_tool, may_manage, DEVELOPER_TOOL_RULE};
    use crate::toolgate::KIND;

    const SHELL: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 9,
        system: false,
    };
    const OWNER: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 0,
        system: false,
    };
    /// Another developer tool's task (the Playground): labelled, not a shell.
    const TOOL: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 12,
        system: false,
    };

    #[test]
    fn only_a_core_package_may_be_the_shell() {
        assert_eq!(shell_install(true, true), Ok(()));
        assert_eq!(shell_install(true, false), Err(SHELL_CORE_ONLY));
        assert_eq!(shell_install(false, false), Ok(()));
        assert_eq!(shell_install(false, true), Ok(()));
    }

    #[test]
    fn a_row_is_a_shell_only_as_a_core_developer_tool() {
        assert!(row_is_shell(KIND, true, true));
        assert!(!row_is_shell(KIND, false, true), "no flag");
        assert!(!row_is_shell(KIND, true, false), "not the image's");
        assert!(!row_is_shell("", true, true), "an application row");
        assert!(!row_is_shell("Developer-Tool", true, true));
    }

    #[test]
    fn the_recorded_shell_flag_needs_the_manifest_and_the_image() {
        let text = |app: &str| {
            alloc::format!(
                "[app]\nname = \"T\"\nsystem_name = \"os.lazy.t\"\nauthor = \"A\"\n\
                 version = \"1.0.0\"\n{app}\n[entry]\nbinary = \"bin/app.elf\"\n"
            )
        };
        let shell = lazypkg::parse_manifest(&text("kind = \"developer-tool\"\nshell = true"))
            .expect("valid");
        let tool = lazypkg::parse_manifest(&text("kind = \"developer-tool\"")).expect("valid");
        assert!(shell_of(&shell, true));
        assert!(!shell_of(&shell, false));
        assert!(!shell_of(&tool, true));
    }

    #[test]
    fn a_shell_tool_is_the_session_owner_and_nothing_more() {
        // Refused as a labelled caller; allowed once judged as its owner.
        assert!(may_manage(&SHELL).is_err() && may_inspect(&SHELL).is_err());
        let owner = as_session_owner(SHELL, true);
        assert_eq!(owner, OWNER);
        assert!(may_manage(&owner).is_ok() && may_inspect(&owner).is_ok());
        // A developer tool still needs a system caller, and a shell is none.
        assert_eq!(may_install_developer_tool(&owner), Err(DEVELOPER_TOOL_RULE));
        // A stamp that claimed a capability would not make a shell a service.
        let claims = Caller {
            system: true,
            ..SHELL
        };
        let judged = as_session_owner(claims, true);
        assert!(!judged.system && judged.label_id == 0);
        assert!(may_install_developer_tool(&judged).is_err());
    }

    #[test]
    fn every_other_caller_is_judged_as_before() {
        // A tool that is not a shell stays refused everywhere a label is.
        assert_eq!(as_session_owner(TOOL, false), TOOL);
        assert!(may_manage(&as_session_owner(TOOL, false)).is_err());
        assert!(may_inspect(&as_session_owner(TOOL, false)).is_err());
        assert!(may_install_developer_tool(&as_session_owner(TOOL, false)).is_err());
        // An unlabelled caller is untouched, whatever the flag says.
        assert_eq!(as_session_owner(OWNER, true), OWNER);
        let system = Caller {
            uid: 0,
            session: 0,
            label_id: 0,
            system: true,
        };
        assert_eq!(as_session_owner(system, true), system);
    }
}
