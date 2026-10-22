//! The reserved `os.lazy.*` namespace (docs/webd-plan.md, "Reserve the webd
//! names").
//!
//! A task labelled `app:<system_name>` owns the services `app.<system_name>.*`
//! and a client resolves such a name to whoever holds the label, so the
//! `system_name` of an installed package is an identity other apps trust
//! (`app.os.lazy.webd.view` is the web engine because the package called
//! `os.lazy.webd` is). Nothing in the manifest grammar keeps a user package
//! from choosing such a name on an image that does not ship the core one, so
//! `pkgd` does: **a package named in the namespace comes only from the image.**
//!
//! The namespace is the bare `os.lazy` and everything below the dotted prefix
//! `os.lazy.`. `os.lazyfoo.x` and `org.lazy.doom` are outside it. No case or
//! separator variants need handling: a `system_name` is lowercase letters,
//! digits and `-` in labels separated by `.` (`layout::valid_system_name`,
//! `lazypkg`'s grammar), and the callers here run after the manifest
//! validated, so `OS.LAZY.x` or `os..lazy.x` never reach the check. The bare
//! `os.lazy` fails the grammar's three-label minimum, and is named here anyway
//! so the rule reads as the namespace, not as a side effect of the grammar.
//!
//! Core provisioning (`provision`) never asks: it installs what the image
//! ships through `install_package`, not through the user-facing install this
//! module gates. What a gate lets through:
//!
//! * [`install`]: the name the image ships (a core update, judged by the
//!   core-app rules), or a name that already has a row. The second case is a
//!   core package the image dropped, which `provision::Action::Demote` leaves
//!   installed as a user app: it keeps its updates. A fresh install of an
//!   unshipped, unrecorded name is refused.
//! * [`develop`]: nothing. A development run owns the app's names and topics
//!   (`dev:<system_name>`), so it would take the service names just the same.

use alloc::format;
use alloc::string::String;

/// The reserved namespace: the bare name and the prefix below it.
const NAMESPACE: &str = "os.lazy";

/// Whether `system_name` is `os.lazy` or below it (`os.lazy.` + anything).
pub fn is_reserved(system_name: &str) -> bool {
    system_name
        .strip_prefix(NAMESPACE)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

/// The refusal text, shared so `Install` and `Develop` say the same thing.
fn refusal(system_name: &str) -> String {
    format!(
        "the name {system_name} is reserved for LazyOS: only packages that ship with the system \
         may be named {NAMESPACE}.*"
    )
}

/// Whether an install of `system_name` may go ahead as far as the namespace
/// is concerned. `shipped`: the image ships a package of that name
/// (`Pkgd::is_core`); `recorded`: the registry already has a row for it.
pub fn install(system_name: &str, shipped: bool, recorded: bool) -> Result<(), String> {
    if is_reserved(system_name) && !shipped && !recorded {
        Err(refusal(system_name))
    } else {
        Ok(())
    }
}

/// Whether a development run of `system_name` may go ahead. A shipped name is
/// refused earlier with its own text (a built-in app is never run as a
/// development build); this refuses the rest of the namespace.
pub fn develop(system_name: &str) -> Result<(), String> {
    if is_reserved(system_name) {
        Err(refusal(system_name))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provision::{plan, Action, Current};

    #[test]
    fn the_namespace_is_the_bare_name_and_the_dotted_prefix() {
        for name in ["os.lazy", "os.lazy.webd", "os.lazy.x.y", "os.lazy.a-b.c"] {
            assert!(is_reserved(name), "{name}");
        }
        for name in [
            "org.lazy.doom",
            "os.lazyfoo.x",
            "os.lazy-x.y",
            "os.lazyos.webd",
            "os.laz.y.x",
            "xos.lazy.x",
            "com.example.os.lazy.x",
            "os",
            "",
        ] {
            assert!(!is_reserved(name), "{name}");
        }
    }

    #[test]
    fn variants_the_grammar_forbids_never_reach_the_rule() {
        // Documented in the module comment: the manifest validator refuses
        // these before any gate runs, so the rule need not.
        for name in ["OS.LAZY.webd", "os.lazy..webd", "os.lazy.", "os lazy webd"] {
            assert!(!crate::layout::valid_system_name(name), "{name}");
        }
    }

    #[test]
    fn a_fresh_user_install_in_the_namespace_is_refused() {
        for name in ["os.lazy.webd", "os.lazy.x.y", "os.lazy"] {
            let why = install(name, false, false).expect_err(name);
            assert!(why.contains(name) && why.contains("reserved for LazyOS"));
            assert_eq!(develop(name).expect_err(name), why);
        }
    }

    #[test]
    fn names_outside_the_namespace_install_freely() {
        for name in ["org.lazy.doom", "os.lazyfoo.x", "com.example.app"] {
            assert_eq!(install(name, false, false), Ok(()));
            assert_eq!(develop(name), Ok(()));
        }
    }

    #[test]
    fn a_shipped_name_is_left_to_the_core_app_rules() {
        assert_eq!(install("os.lazy.webd", true, true), Ok(()));
        assert_eq!(install("os.lazy.webd", true, false), Ok(()));
    }

    #[test]
    fn core_provisioning_is_not_gated() {
        // Provisioning installs what `plan` says, by `install_package`; the
        // plan is the same with or without this module.
        let shipped = [crate::provision::Shipped {
            system_name: "os.lazy.webd".into(),
            version: "1.0.0".into(),
            digest: "0".repeat(64),
            autostart: false,
        }];
        assert_eq!(
            plan(&shipped, &[]),
            [Action::Install("os.lazy.webd".into())]
        );
    }

    #[test]
    fn a_demoted_core_package_keeps_its_updates() {
        // The image dropped it: the row stays, as a user app.
        let row = Current {
            system_name: "os.lazy.retired".into(),
            version: "1.0.0".into(),
            digest: "1".repeat(64),
            core: true,
            machine_wide: true,
        };
        assert_eq!(
            plan(&[], &[row]),
            [Action::Demote("os.lazy.retired".into())]
        );
        // No longer shipped, still recorded: an update is not a new name.
        assert_eq!(install("os.lazy.retired", false, true), Ok(()));
        // Once the row is removed, reinstalling is a fresh user install.
        assert!(install("os.lazy.retired", false, false).is_err());
    }
}
