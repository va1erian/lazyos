//! The permission explanation table: one plain-language sentence and a risk
//! for everything a package can ask for, so every installer shows the same
//! words.
//!
//! The table is keyed by MIDL interface name. A test reads `idl/manifest.json`
//! and fails when an interface is declared in `idl/` without a row here, so a
//! new service cannot ship without somebody deciding how it is described to the
//! person approving an install. An interface the table does not know (a typo,
//! or another app's) is `high` risk with a sentence that says so.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_pkgd_v1::Permission;

use crate::resident;

/// A risk word (`low`, `medium`, `high`) and the sentence shown for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explained {
    pub risk: &'static str,
    pub text: String,
}

pub const LOW: &str = "low";
pub const MEDIUM: &str = "medium";
pub const HIGH: &str = "high";

mod interfaces;

pub use interfaces::{DEVELOP, INTERFACES};

/// The explanation of one requested interface.
pub fn interface(name: &str) -> Explained {
    for (known, risk, text) in INTERFACES {
        if *known == name {
            return Explained {
                risk,
                text: String::from(*text),
            };
        }
    }
    Explained {
        risk: HIGH,
        text: format!("An interface this system does not know: {name}"),
    }
}

/// The explanation of one `publish:<pattern>` or `subscribe:<pattern>` entry.
/// The risk follows the pattern's first segment: `system/` is the platform's
/// own traffic (high), `app/` is application-to-application (low), the rest is
/// shared (medium).
pub fn topic(entry: &str) -> Explained {
    let (verb, pattern) = match entry.split_once(':') {
        Some(("publish", pattern)) => ("Send messages on", pattern),
        Some(("subscribe", pattern)) => ("Listen to messages on", pattern),
        _ => ("Use the topic", entry),
    };
    let risk = match pattern.split('/').next() {
        Some("system") => HIGH,
        Some("app") => LOW,
        _ => MEDIUM,
    };
    Explained {
        risk,
        text: format!("{verb} the topic {pattern}"),
    }
}

/// Where per-user data lives; rules inside it are the gentler ones.
const HOME: &str = fhs::state::HOME_ROOT;

/// The per-user folder of an app inside `$HOME`: `$HOME/.apps/<system_name>`.
const HOME_APPS: &str = ".apps";

/// `path` is `root` or below it.
fn within(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Where a file rule points, which sets its risk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The app's own folder, `$HOME/.apps/<own system_name>/...`.
    OwnData,
    /// The user's personal data: anything else under `$HOME` or `/home`.
    Personal,
    /// Anywhere else on the system.
    System,
}

fn place(path: &str, system_name: &str) -> Place {
    let own = format!("{}/{HOME_APPS}/{system_name}", lazypkg::HOME_VAR);
    if within(path, &own) {
        Place::OwnData
    } else if within(path, lazypkg::HOME_VAR) || within(path, HOME) {
        Place::Personal
    } else {
        Place::System
    }
}

/// The explanation of one `read:<path>` or `write:<path>` entry of the package
/// `system_name`. The app's own `$HOME/.apps/<system_name>` folder is low risk
/// either way. Other personal data (`$HOME/...`, `/home/...`) is low to
/// read and medium to write; anywhere else it is medium to read and high to
/// write.
pub fn file(entry: &str, system_name: &str) -> Explained {
    match entry.split_once(':') {
        Some(("write", path)) => Explained {
            risk: match place(path, system_name) {
                Place::OwnData => LOW,
                Place::Personal => MEDIUM,
                Place::System => HIGH,
            },
            text: format!("Create, change and delete files in {path}"),
        },
        Some(("read", path)) => Explained {
            risk: match place(path, system_name) {
                Place::OwnData | Place::Personal => LOW,
                Place::System => MEDIUM,
            },
            text: format!("Read files in {path}"),
        },
        _ => Explained {
            risk: HIGH,
            text: format!("An unknown file permission: {entry}"),
        },
    }
}

/// The explanation of one `network` entry (`outbound` is the only known one).
pub fn network(entry: &str) -> Explained {
    if entry == "outbound" {
        Explained {
            risk: HIGH,
            text: String::from("Connect to other computers over the network"),
        }
    } else {
        Explained {
            risk: HIGH,
            text: format!("An unknown network permission: {entry}"),
        }
    }
}

fn permission(kind: &str, value: &str, explained: Explained) -> Permission {
    Permission {
        kind: String::from(kind),
        value: String::from(value),
        risk: String::from(explained.risk),
        explanation: explained.text,
    }
}

/// Every permission of `manifest` as the consent screen lists it: interfaces,
/// then topics, files and network, each in manifest order, one entry per
/// request, then `develop` and `resident` when asked for. The interfaces and
/// topics `resident` implies follow the manifest's own ([`resident`]).
pub fn permissions(manifest: &Manifest) -> Vec<Permission> {
    let requested = &manifest.permissions;
    let mut out = Vec::new();
    for name in resident::interfaces(manifest) {
        out.push(permission("interface", name, interface(name)));
    }
    for entry in resident::topics(manifest) {
        out.push(permission("topic", entry, topic(entry)));
    }
    for entry in &requested.files {
        let explained = file(entry, &manifest.app.system_name);
        out.push(permission("file", entry, explained));
    }
    for entry in &requested.network {
        out.push(permission("network", entry, network(entry)));
    }
    if requested.develop {
        let explained = Explained {
            risk: HIGH,
            text: String::from(DEVELOP),
        };
        out.push(permission("develop", "true", explained));
    }
    if manifest.entry.resident {
        let explained = Explained {
            risk: LOW,
            text: String::from(resident::RESIDENT),
        };
        out.push(permission("resident", "true", explained));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `"interface": "<name>"` in the checked-in manifest `midlc`
    /// writes next to the `.midl` files.
    fn declared_interfaces() -> Vec<String> {
        let manifest = include_str!("../../../idl/manifest.json");
        let key = "\"interface\": \"";
        let mut names = Vec::new();
        let mut rest = manifest;
        while let Some(at) = rest.find(key) {
            rest = &rest[at + key.len()..];
            let end = rest.find('"').expect("closing quote");
            names.push(String::from(&rest[..end]));
        }
        names
    }

    #[test]
    fn the_table_covers_every_idl_interface() {
        let declared = declared_interfaces();
        assert!(declared.len() >= 20, "{declared:?}");
        for name in &declared {
            assert!(
                INTERFACES.iter().any(|(known, _, _)| known == name),
                "{name} is declared in idl/ but has no explanation in pkgstore::explain"
            );
        }
    }

    #[test]
    fn the_table_names_only_declared_interfaces_and_has_no_duplicates() {
        let declared = declared_interfaces();
        for (index, (name, risk, text)) in INTERFACES.iter().enumerate() {
            assert!(declared.iter().any(|d| d == name), "{name} is not in idl/");
            assert!([LOW, MEDIUM, HIGH].contains(risk), "{name}: {risk}");
            assert!(!text.is_empty() && !text.ends_with('.'), "{name}: {text}");
            assert!(
                INTERFACES[..index]
                    .iter()
                    .all(|(other, _, _)| other != name),
                "{name} listed twice"
            );
        }
    }

    #[test]
    fn known_interfaces_get_their_row() {
        let clipboard = interface("os.lazy.clipboard.v1");
        assert_eq!(clipboard.risk, MEDIUM);
        assert_eq!(clipboard.text, "Read and change what you copy and paste");
        assert_eq!(interface("os.lazy.keyd.v1").risk, HIGH);
        assert_eq!(interface("os.lazy.display.v1").risk, LOW);
    }

    #[test]
    fn an_unknown_interface_is_high_risk_and_says_so() {
        let unknown = interface("com.evil.thing.v1");
        assert_eq!(unknown.risk, HIGH);
        assert_eq!(
            unknown.text,
            "An interface this system does not know: com.evil.thing.v1"
        );
    }

    #[test]
    fn topic_risk_follows_the_prefix() {
        assert_eq!(topic("subscribe:system/events/open/+").risk, HIGH);
        assert_eq!(topic("publish:app/org.lazy.paint/#").risk, LOW);
        assert_eq!(topic("publish:session/1/clipboard/changed").risk, MEDIUM);
        let listen = topic("subscribe:system/stats/memory");
        assert_eq!(
            listen.text,
            "Listen to messages on the topic system/stats/memory"
        );
        assert!(topic("publish:app/x/y")
            .text
            .starts_with("Send messages on"));
    }

    #[test]
    fn file_risk_depends_on_read_write_and_location() {
        let file = |entry| file(entry, "org.lazy.demo");
        assert_eq!(file("read:/home/*/pictures").risk, LOW);
        assert_eq!(file("write:/home/*/pictures").risk, MEDIUM);
        assert_eq!(file("read:/etc/passwd").risk, MEDIUM);
        assert_eq!(file("write:/apps/*").risk, HIGH);
        // A sibling that merely starts with the home path is not inside it.
        assert_eq!(file("write:/homework").risk, HIGH);
        assert_eq!(file("exec:/bin/sh").risk, HIGH);
    }

    #[test]
    fn home_rules_are_personal_data_except_the_apps_own_folder() {
        let file = |entry| file(entry, "org.lazy.demo");
        assert_eq!(file("write:$HOME/.apps/org.lazy.demo").risk, LOW);
        assert_eq!(file("write:$HOME/.apps/org.lazy.demo/data/*").risk, LOW);
        assert_eq!(file("read:$HOME/.apps/org.lazy.demo/data").risk, LOW);
        // Another app's folder, or one whose name merely starts with ours.
        assert_eq!(file("write:$HOME/.apps/org.lazy.other/x").risk, MEDIUM);
        assert_eq!(file("write:$HOME/.apps/org.lazy.demo2/x").risk, MEDIUM);
        assert_eq!(file("write:$HOME/.apps/*").risk, MEDIUM);
        assert_eq!(file("write:$HOME/Documents/*").risk, MEDIUM);
        assert_eq!(file("read:$HOME/Documents/*").risk, LOW);
        assert_eq!(
            file("write:$HOME/Documents/*").text,
            "Create, change and delete files in $HOME/Documents/*"
        );
    }

    #[test]
    fn network_outbound_is_high() {
        assert_eq!(network("outbound").risk, HIGH);
        assert_eq!(network("inbound").risk, HIGH);
    }

    #[test]
    fn permissions_lists_one_entry_per_request_in_a_stable_order() {
        let manifest = lazypkg::parse_manifest(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n\
             [permissions]\ninterfaces = [\"os.lazy.clipboard.v1\", \"os.lazy.keyd.v1\"]\n\
             topics = [\"subscribe:system/events/open/+\"]\n\
             files = [\"read:$HOME/pictures\"]\nnetwork = [\"outbound\"]\ndevelop = true\n",
        )
        .expect("valid");
        let listed = permissions(&manifest);
        let kinds: Vec<(&str, &str)> = listed
            .iter()
            .map(|p| (p.kind.as_str(), p.risk.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("interface", "medium"),
                ("interface", "high"),
                ("topic", "high"),
                ("file", "low"),
                ("network", "high"),
                ("develop", "high"),
            ]
        );
        assert!(listed.iter().all(|p| p.kind != "resident"));
        assert_eq!(listed[0].value, "os.lazy.clipboard.v1");
        assert_eq!(
            listed[0].explanation,
            "Read and change what you copy and paste"
        );
    }

    fn rows(manifest: &str) -> Vec<(String, String, String, String)> {
        let text = alloc::format!(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n{manifest}"
        );
        let manifest = lazypkg::parse_manifest(&text).expect("valid");
        permissions(&manifest)
            .into_iter()
            .map(|p| (p.kind, p.value, p.risk, p.explanation))
            .collect()
    }

    #[test]
    fn resident_lists_its_implied_permissions_and_its_own_line() {
        let row = |kind: &str, value: &str, explained: Explained| {
            (
                kind.into(),
                value.into(),
                explained.risk.into(),
                explained.text,
            )
        };
        let listed = rows("resident = true\n[permissions]\ninterfaces = [\"os.lazy.audio.v1\"]\n");
        assert_eq!(
            listed,
            [
                row(
                    "interface",
                    "os.lazy.audio.v1",
                    interface("os.lazy.audio.v1")
                ),
                row(
                    "interface",
                    "os.lazy.shell.tray.v1",
                    interface("os.lazy.shell.tray.v1")
                ),
                row(
                    "interface",
                    "os.lazy.init.app.v1",
                    interface("os.lazy.init.app.v1")
                ),
                row(
                    "topic",
                    "subscribe:session/+/shell/tray",
                    topic("subscribe:session/+/shell/tray")
                ),
                (
                    "resident".into(),
                    "true".into(),
                    LOW.into(),
                    "Keeps running in the background and shows an icon in the taskbar".into()
                ),
            ]
        );
        // The implied interfaces have their own rows in the table.
        assert_eq!(
            listed[1].3,
            "Show an icon in the taskbar, with a tooltip and a menu"
        );
    }

    #[test]
    fn resident_does_not_repeat_what_the_manifest_lists() {
        let listed = rows(
            "resident = true\n[permissions]\ninterfaces = [\"os.lazy.shell.tray.v1\"]\n\
             topics = [\"subscribe:session/+/shell/tray\"]\n",
        );
        let values: Vec<&str> = listed.iter().map(|row| row.1.as_str()).collect();
        assert_eq!(
            values,
            [
                "os.lazy.shell.tray.v1",
                "os.lazy.init.app.v1",
                "subscribe:session/+/shell/tray",
                "true"
            ]
        );
    }

    #[test]
    fn a_non_resident_manifest_lists_only_its_own_entries() {
        let listed = rows("resident = false\n[permissions]\ninterfaces = [\"os.lazy.audio.v1\"]\n");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1, "os.lazy.audio.v1");
    }
}
