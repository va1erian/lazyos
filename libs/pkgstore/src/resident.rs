//! What `[entry] resident = true` implies (docs/tray-plan.md §5, "Grants and
//! consent").
//!
//! A resident app runs with no window and keeps an icon in the taskbar, so it
//! needs the tray and `init`'s lifecycle channel whatever else it does. The
//! manifest's `interfaces` stays about what the app does: `resident` brings
//! those permissions itself. They are still *requested* permissions, never a
//! baseline: [`crate::explain::permissions`] lists them on the consent screen
//! (they count toward [`crate::inspect::MAX_PERMISSIONS`]) and
//! [`crate::rules::compile`] emits their rules from the manifest, so only an
//! `Install` the user approved loads them.
//!
//! [`interfaces`] and [`topics`] are the one place both read the requested
//! lists from: the manifest's own entries in order, then each implied entry
//! the manifest does not already list (one consent row, no duplicate rule).

use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_init_app_v1 as init_app;
use messenger_generated::os_lazy_shell_tray_v1 as tray;

/// The interfaces a resident app is given: its tray icon and `init`'s
/// lifecycle channel (`Watch`, then `Reopen` and `Quit`).
pub const INTERFACES: [&str; 2] = [tray::INTERFACE_NAME, init_app::INTERFACE_NAME];

/// The topics a resident app is given. The plan names only the two
/// interfaces; this one is a T3 decision: the tray client library follows the
/// shell's retained tray generation (`session/<id>/shell/tray`, `tray.midl`)
/// to call `Set` again after a shell restart, and a labelled app cannot
/// subscribe to it without the rule. Equal to the generated
/// `os_lazy_shell_tray_v1::TOPIC_SESSION_SHELL_TRAY` (a test checks it).
pub const TOPICS: [&str; 1] = ["subscribe:session/+/shell/tray"];

/// The consent line of `resident = true` itself.
pub const RESIDENT: &str = "Keeps running in the background and shows an icon in the taskbar";

/// Every requested interface of `manifest`: its own, then the implied ones
/// it does not list.
pub fn interfaces(manifest: &Manifest) -> Vec<&str> {
    with_implied(
        &manifest.permissions.interfaces,
        implied(manifest, &INTERFACES),
    )
}

/// Every requested topic of `manifest`: its own, then the implied ones it
/// does not list.
pub fn topics(manifest: &Manifest) -> Vec<&str> {
    with_implied(&manifest.permissions.topics, implied(manifest, &TOPICS))
}

/// The entries `resident` adds, none when the app is not resident.
fn implied<'a>(manifest: &Manifest, entries: &'a [&'a str]) -> &'a [&'a str] {
    if manifest.entry.resident {
        entries
    } else {
        &[]
    }
}

fn with_implied<'a>(own: &'a [String], implied: &[&'a str]) -> Vec<&'a str> {
    let mut out: Vec<&str> = own.iter().map(|entry| entry.as_str()).collect();
    for entry in implied {
        if !own.iter().any(|listed| listed == entry) {
            out.push(entry);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn manifest(entry: &str, permissions: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n{entry}[permissions]\n{permissions}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    #[test]
    fn the_topic_is_the_declared_tray_generation() {
        let declared = format!("subscribe:{}", tray::TOPIC_SESSION_SHELL_TRAY);
        assert_eq!(TOPICS, [declared.as_str()]);
        assert_eq!(INTERFACES, ["os.lazy.shell.tray.v1", "os.lazy.init.app.v1"]);
    }

    #[test]
    fn a_resident_app_gets_the_implied_entries_after_its_own() {
        let resident = manifest(
            "resident = true\n",
            "interfaces = [\"os.lazy.audio.v1\"]\ntopics = [\"subscribe:app/x/y\"]\n",
        );
        assert_eq!(
            interfaces(&resident),
            [
                "os.lazy.audio.v1",
                "os.lazy.shell.tray.v1",
                "os.lazy.init.app.v1"
            ]
        );
        assert_eq!(
            topics(&resident),
            ["subscribe:app/x/y", "subscribe:session/+/shell/tray"]
        );
    }

    #[test]
    fn an_implied_entry_the_manifest_lists_is_not_repeated() {
        let resident = manifest(
            "resident = true\n",
            "interfaces = [\"os.lazy.init.app.v1\"]\ntopics = [\"subscribe:session/+/shell/tray\"]\n",
        );
        assert_eq!(
            interfaces(&resident),
            ["os.lazy.init.app.v1", "os.lazy.shell.tray.v1"]
        );
        assert_eq!(topics(&resident), ["subscribe:session/+/shell/tray"]);
    }

    #[test]
    fn a_non_resident_app_requests_only_what_it_lists() {
        let plain = manifest("", "interfaces = [\"os.lazy.audio.v1\"]\n");
        assert_eq!(interfaces(&plain), ["os.lazy.audio.v1"]);
        assert!(topics(&plain).is_empty());
        let explicit = manifest("resident = false\n", "");
        assert!(interfaces(&explicit).is_empty());
    }
}
