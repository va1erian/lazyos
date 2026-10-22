//! Where a developer tool may come from, and when `pkgd` and `init` load one
//! (`docs/devtool-packages-plan.md` section 4, phase T3).
//!
//! A developer tool runs with the user's own authority, so it is accepted
//! from two places only: the image (a core package, installed by `pkgd`'s own
//! provisioning) and an administrator, who approved it on `elevd`'s trusted
//! prompt as the operation `pkg.install-devtool`. Both leave the same mark on
//! the app's row in `confd`: `kind` is `developer-tool` and the row is
//! machine-wide (installed by a system caller) or core. Nothing else writes
//! that row (`sys/**` needs a system caller), so the mark *is* the record of
//! the approval, and every reader checks it again: [`loadable`] at `pkgd`'s
//! boot reconciliation, [`row_approved`] in `init`. A tool package put into
//! `/apps` by other means has no such row and is never loaded.
//!
//! [`install`] is the decision at install time, from the route the request
//! took ([`Route`]) and the kernel-stamped caller.

use crate::access::{self, Caller};

/// The `Installed::kind` of a developer tool's row; an application's is empty.
pub const KIND: &str = devtool::KIND;

/// Which door an install came through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
    /// `Install`.
    Plain,
    /// `InstallApproved` from `elevd`; `devtool` is what the administrator
    /// approved (`pkg.install-devtool`), `false` for `pkg.install` and
    /// `pkg.update-core`.
    Approved { devtool: bool },
    /// `InstallDebug` from `dbgd` on a box built for remote control.
    Debug,
}

/// The refusal of a developer tool at the door of a plain `pkg.install` (or
/// `pkg.update-core`): the administrator would not have seen its warning.
pub const NEEDS_DEVTOOL_OPERATION: &str =
    "that package is a developer tool: an administrator must approve it as pkg.install-devtool";
/// The refusal of `pkg.install-devtool` for a package that is not one.
pub const NOT_A_DEVELOPER_TOOL: &str =
    "that package is not a developer tool: install it with pkg.install";
/// The refusal of a plain `Install` of a tool by `elevd`: it installs only
/// what was approved, through `InstallApproved`.
pub const ELEVD_APPROVED_ONLY: &str = "elevd installs a developer tool only as pkg.install-devtool";

/// Whether an install of a package (`is_tool`: a developer tool) through
/// `route` by `caller` may go ahead. `by_elevd` is `caller` being `elevd`
/// (which is also `caller.system`). A package that is not a tool is judged by
/// the callers' ordinary rules, not here.
///
/// ```text
/// tool + Plain             only a system service that is not elevd
/// tool + Approved{false}   never (the warning was not shown)
/// tool + Approved{true}    elevd, system caller
/// tool + Debug             dbgd's request, made a system caller by pkgd
/// plain package + Approved{true}   never
/// ```
pub fn install(
    is_tool: bool,
    route: Route,
    caller: &Caller,
    by_elevd: bool,
) -> Result<(), &'static str> {
    match (is_tool, route) {
        (false, Route::Approved { devtool: true }) => Err(NOT_A_DEVELOPER_TOOL),
        (false, _) => Ok(()),
        (true, Route::Approved { devtool: false }) => Err(NEEDS_DEVTOOL_OPERATION),
        (true, Route::Plain) if by_elevd => Err(ELEVD_APPROVED_ONLY),
        (true, _) => access::may_install_developer_tool(caller),
    }
}

/// The refusal of an approved install that would change a core package's
/// class: a core application never becomes a developer tool, nor a core
/// tool an application. (An update that keeps the class is allowed: an
/// administrator updates a core developer tool with `pkg.install-devtool`.)
pub const CORE_CLASS_CHANGE: &str =
    "a core package cannot change between an application and a developer tool";
/// The refusal of an approved install that would add or drop `shell = true`
/// on a core developer tool. The shell role keeps the credential denies off
/// the label and makes `pkgd` and `init` treat what runs under it as the
/// session's owner; the administrator's prompt names the tool, not that
/// change, so a core tool without it (the Playground) cannot gain it by an
/// update, and the Terminal cannot lose it silently either.
pub const CORE_SHELL_CHANGE: &str =
    "a core developer tool cannot gain or lose the shell role by an update";

/// Whether an approved install may replace the installed core package:
/// `was_tool` is the class the installed copy recorded, `is_tool` the new
/// package's, `was_shell` and `is_shell` the shell role of each. Both must
/// be the same.
pub fn core_class_change(
    was_tool: bool,
    is_tool: bool,
    was_shell: bool,
    is_shell: bool,
) -> Result<(), &'static str> {
    if was_tool != is_tool {
        Err(CORE_CLASS_CHANGE)
    } else if was_shell != is_shell {
        Err(CORE_SHELL_CHANGE)
    } else {
        Ok(())
    }
}

/// The `system_name` of the tool whose label `label` is (`tool:<system_name>`);
/// `None` for any other label.
pub fn tool_system_name(label: &str) -> Option<&str> {
    devtool::tool_system_name(label)
}

/// Whether the `kind` a row records is a developer tool's.
pub fn is_tool_kind(kind: &str) -> bool {
    kind == KIND
}

/// The `Installed::kind` for a manifest: the class spelling, or empty.
pub fn kind_of(manifest: &lazypkg::Manifest) -> &'static str {
    if manifest.is_developer_tool() {
        KIND
    } else {
        ""
    }
}

/// Whether a row with this `kind` may be used: anything but a tool, or a tool
/// installed by a system caller (`machine_wide`) or shipped by the image
/// (`core`).
pub fn row_approved(kind: &str, machine_wide: bool, core: bool) -> bool {
    !is_tool_kind(kind) || machine_wide || core
}

/// Why `pkgd` will not load a row's app at boot. `Ok` means load it.
///
/// The row (written by `pkgd` at install time from the checked manifest) and
/// the stored manifest (a file under `/apps`) must agree on the class, so a
/// manifest edited after the install cannot turn an application into a tool;
/// a tool also needs the approval mark ([`row_approved`]).
pub fn loadable(
    row_kind: &str,
    manifest_is_tool: bool,
    machine_wide: bool,
    core: bool,
) -> Result<(), &'static str> {
    if is_tool_kind(row_kind) != manifest_is_tool {
        return Err("the stored manifest and the install record disagree about the package class");
    }
    if !row_approved(row_kind, machine_wide, core) {
        return Err("a developer tool is loaded only after an administrator approved it");
    }
    Ok(())
}

/// The whole boot-time decision for an installed row and its stored
/// manifest, shared by `pkgd`'s reconciliation (which loads the label's
/// rules) and `init`'s app list (which starts the app under that label): the
/// class must agree and a tool must be approved ([`loadable`]), and a
/// manifest that claims the shell role must belong to a package the image
/// ships ([`crate::shellrole::shell_install`], which also lifts the label's
/// credential denies). `Ok` means the app may be loaded and started.
pub fn stored_loadable(
    row_kind: &str,
    manifest: &lazypkg::Manifest,
    machine_wide: bool,
    core: bool,
) -> Result<(), &'static str> {
    loadable(row_kind, manifest.is_developer_tool(), machine_wide, core)
        .and(crate::shellrole::shell_install(manifest.is_shell(), core))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ELEVD: Caller = Caller {
        uid: 906,
        session: 0,
        label_id: 0,
        system: true,
    };
    const SERVICE: Caller = Caller {
        uid: 0,
        session: 0,
        label_id: 0,
        system: true,
    };
    const OWNER: Caller = Caller {
        uid: 1000,
        session: 3,
        label_id: 0,
        system: false,
    };
    const ADMIN: Caller = Caller {
        uid: 1001,
        session: 4,
        label_id: 0,
        system: false,
    };
    const LABELLED: Caller = Caller {
        uid: 0,
        session: 3,
        label_id: 7,
        system: true,
    };

    fn run(is_tool: bool, route: Route, caller: &Caller) -> Result<(), &'static str> {
        install(is_tool, route, caller, *caller == ELEVD)
    }

    #[test]
    fn a_tool_is_installed_only_as_the_operation_an_administrator_saw() {
        let approved = Route::Approved { devtool: true };
        assert_eq!(run(true, approved, &ELEVD), Ok(()));
        // The plain operations never carry the warning.
        assert_eq!(
            run(true, Route::Approved { devtool: false }, &ELEVD),
            Err(NEEDS_DEVTOOL_OPERATION)
        );
        // And the devtool operation is only for tools.
        assert_eq!(run(false, approved, &ELEVD), Err(NOT_A_DEVELOPER_TOOL));
        assert!(NEEDS_DEVTOOL_OPERATION.contains("pkg.install-devtool"));
        assert!(NOT_A_DEVELOPER_TOOL.contains("pkg.install"));
    }

    #[test]
    fn a_session_owner_never_installs_a_tool_directly() {
        for caller in [OWNER, ADMIN] {
            for route in [
                Route::Plain,
                Route::Approved { devtool: true },
                Route::Debug,
            ] {
                assert_eq!(
                    run(true, route, &caller),
                    Err(access::DEVELOPER_TOOL_RULE),
                    "{caller:?} {route:?}"
                );
            }
        }
        // A labelled task is refused even when it claims to be a system caller.
        for route in [
            Route::Plain,
            Route::Approved { devtool: true },
            Route::Debug,
        ] {
            let refusal = run(true, route, &LABELLED).unwrap_err();
            assert!(refusal.contains("applications"), "{refusal}");
        }
    }

    #[test]
    fn a_system_service_and_dbgd_install_a_tool_but_elevd_only_when_approved() {
        assert_eq!(run(true, Route::Plain, &SERVICE), Ok(()));
        assert_eq!(run(true, Route::Plain, &ELEVD), Err(ELEVD_APPROVED_ONLY));
        // `InstallDebug` reaches here with `pkgd` having made dbgd a system
        // caller for the one request.
        assert_eq!(run(true, Route::Debug, &SERVICE), Ok(()));
    }

    #[test]
    fn a_package_that_is_not_a_tool_is_not_judged_here() {
        for caller in [OWNER, ELEVD, SERVICE, LABELLED] {
            for route in [
                Route::Plain,
                Route::Approved { devtool: false },
                Route::Debug,
            ] {
                assert_eq!(run(false, route, &caller), Ok(()));
            }
        }
    }

    #[test]
    fn a_core_package_keeps_its_class() {
        assert_eq!(
            core_class_change(true, true, false, false),
            Ok(()),
            "a core tool updated"
        );
        assert_eq!(
            core_class_change(false, false, false, false),
            Ok(()),
            "a core app updated"
        );
        assert_eq!(
            core_class_change(false, true, false, false),
            Err(CORE_CLASS_CHANGE)
        );
        assert_eq!(
            core_class_change(true, false, false, false),
            Err(CORE_CLASS_CHANGE)
        );
    }

    #[test]
    fn a_core_tool_keeps_its_shell_role() {
        // The Terminal updated by an administrator stays a shell.
        assert_eq!(core_class_change(true, true, true, true), Ok(()));
        // The Playground cannot become a shell by an update, nor the Terminal
        // stop being one: the prompt would not say either.
        assert_eq!(
            core_class_change(true, true, false, true),
            Err(CORE_SHELL_CHANGE)
        );
        assert_eq!(
            core_class_change(true, true, true, false),
            Err(CORE_SHELL_CHANGE)
        );
        // A class change is reported as one, whatever the flag says.
        assert_eq!(
            core_class_change(false, true, false, true),
            Err(CORE_CLASS_CHANGE)
        );
    }

    #[test]
    fn a_tool_row_is_used_only_when_it_was_approved() {
        assert!(row_approved("", false, false), "an application row");
        assert!(row_approved(KIND, true, false), "an administrator's");
        assert!(row_approved(KIND, false, true), "the image's");
        assert!(!row_approved(KIND, false, false), "a user's own install");
        // Whatever a row says in another spelling is an application's.
        assert!(row_approved("Developer-Tool", false, false));
        assert!(!is_tool_kind("developer tool"));
    }

    #[test]
    fn boot_loads_a_row_only_when_record_and_manifest_agree_and_it_is_approved() {
        assert_eq!(loadable("", false, false, false), Ok(()));
        assert_eq!(loadable(KIND, true, true, false), Ok(()));
        assert_eq!(loadable(KIND, true, false, true), Ok(()));
        let unapproved = loadable(KIND, true, false, false).unwrap_err();
        assert!(unapproved.contains("administrator"), "{unapproved}");
        // A manifest edited into a tool, or out of one, after the install.
        for (kind, tool) in [("", true), (KIND, false)] {
            let why = loadable(kind, tool, true, true).unwrap_err();
            assert!(why.contains("disagree"), "{why}");
        }
    }

    #[test]
    fn a_stored_manifest_is_loaded_only_when_row_class_approval_and_shell_agree() {
        let text = |app: &str| {
            alloc::format!(
                "[app]\nname = \"T\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
                 version = \"1.0.0\"\n{app}\n[entry]\nbinary = \"bin/app.elf\"\n"
            )
        };
        let parse = |app: &str| lazypkg::parse_manifest(&text(app)).expect("valid");
        let app = parse("");
        let tool = parse("kind = \"developer-tool\"");
        let shell = parse("kind = \"developer-tool\"\nshell = true");
        assert_eq!(stored_loadable("", &app, false, false), Ok(()));
        assert_eq!(stored_loadable(KIND, &tool, true, false), Ok(()));
        assert_eq!(stored_loadable(KIND, &shell, false, true), Ok(()));
        // An administrator's tool whose stored manifest was edited to claim the
        // shell role: refused, so init does not start it under a label pkgd
        // revoked.
        assert!(stored_loadable(KIND, &shell, true, false)
            .unwrap_err()
            .contains("shell"));
        // A tool row whose manifest was edited into an application, and the
        // reverse.
        assert!(stored_loadable(KIND, &app, true, true)
            .unwrap_err()
            .contains("disagree"));
        assert!(stored_loadable("", &tool, true, true)
            .unwrap_err()
            .contains("disagree"));
        // An unapproved tool.
        assert!(stored_loadable(KIND, &tool, false, false)
            .unwrap_err()
            .contains("administrator"));
    }

    #[test]
    fn the_kind_of_a_manifest_is_its_class_spelling() {
        let text = |kind: &str| {
            alloc::format!(
                "[app]\nname = \"T\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
                 version = \"1.0.0\"\n{kind}\n[entry]\nbinary = \"bin/app.elf\"\n"
            )
        };
        let tool = lazypkg::parse_manifest(&text("kind = \"developer-tool\"")).expect("valid");
        let app = lazypkg::parse_manifest(&text("")).expect("valid");
        let named = lazypkg::parse_manifest(&text("kind = \"application\"")).expect("valid");
        assert_eq!(kind_of(&tool), KIND);
        assert_eq!(kind_of(&app), "");
        assert_eq!(kind_of(&named), "");
    }
}
