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

/// A risk word (`low`, `medium`, `high`) and the sentence shown for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explained {
    pub risk: &'static str,
    pub text: String,
}

pub const LOW: &str = "low";
pub const MEDIUM: &str = "medium";
pub const HIGH: &str = "high";

/// `(interface name, risk, sentence)` for every interface in `idl/`.
pub const INTERFACES: &[(&str, &str, &str)] = &[
    (
        "os.lazy.accounts.v1",
        HIGH,
        "Look up user accounts and create new ones",
    ),
    (
        "os.lazy.audio.v1",
        MEDIUM,
        "Play sound through the speakers",
    ),
    (
        "os.lazy.clipboard.v1",
        MEDIUM,
        "Read and change what you copy and paste",
    ),
    ("os.lazy.confd.v1", HIGH, "Read and change system settings"),
    (
        "os.lazy.display.v1",
        LOW,
        "Show its own windows on the desktop",
    ),
    (
        "os.lazy.echo.v1",
        LOW,
        "Use the echo test service, which only repeats what it is sent",
    ),
    (
        "os.lazy.healthd.v1",
        LOW,
        "See whether the system's services are healthy",
    ),
    (
        "os.lazy.init.v1",
        HIGH,
        "Start programs and see which services are running",
    ),
    (
        "os.lazy.input.v1",
        LOW,
        "Receive keyboard input while its window is focused",
    ),
    (
        "os.lazy.input.shell.v1",
        HIGH,
        "Control how keyboard input is routed for the whole desktop",
    ),
    (
        "os.lazy.keyd.v1",
        HIGH,
        "Use the stored secrets and encryption keys",
    ),
    (
        "os.lazy.logd.v1",
        MEDIUM,
        "Read the system event log, which records what other apps did",
    ),
    (
        "os.lazy.logind.v1",
        MEDIUM,
        "See who is logged in and which sessions exist",
    ),
    (
        "os.lazy.mimed.v1",
        MEDIUM,
        "Look up which app opens a file type and open files with other apps",
    ),
    (
        "os.lazy.net.nic.v1",
        HIGH,
        "Control the network card directly",
    ),
    (
        "os.lazy.net.stack.v1",
        HIGH,
        "Read and change the network configuration",
    ),
    (
        "os.lazy.net.socket.v1",
        HIGH,
        "Open network connections to other computers",
    ),
    ("os.lazy.pkgd.v1", HIGH, "Install and remove applications"),
    (
        "os.lazy.messenger.policy.v1",
        HIGH,
        "Change what other apps are allowed to do",
    ),
    (
        "os.lazy.messenger.names.resolve.v1",
        HIGH,
        "Look up any system service by name",
    ),
    (
        "os.lazy.messenger.registry.v1",
        HIGH,
        "Publish system services and list the ones that exist",
    ),
    ("os.lazy.sysmond.v1", LOW, "Read CPU and memory statistics"),
    ("os.lazy.timed.v1", LOW, "Read the time and the time zone"),
    (
        "os.lazy.messenger.topics.v1",
        MEDIUM,
        "Send and receive messages on topics",
    ),
    (
        "os.lazy.messenger.topics.publish.v1",
        HIGH,
        "Pass the system's internal check for sending messages on any topic",
    ),
    (
        "os.lazy.messenger.topics.subscribe.v1",
        HIGH,
        "Pass the system's internal check for listening on any topic",
    ),
];

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
const HOME: &str = "/data/home";

fn in_home(path: &str) -> bool {
    path == HOME
        || path
            .strip_prefix(HOME)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The explanation of one `read:<path>` or `write:<path>` entry. Writing is
/// medium inside `/data/home` and high anywhere else; reading is low inside
/// `/data/home` and medium anywhere else.
pub fn file(entry: &str) -> Explained {
    match entry.split_once(':') {
        Some(("write", path)) => Explained {
            risk: if in_home(path) { MEDIUM } else { HIGH },
            text: format!("Create, change and delete files in {path}"),
        },
        Some(("read", path)) => Explained {
            risk: if in_home(path) { LOW } else { MEDIUM },
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
/// request.
pub fn permissions(manifest: &Manifest) -> Vec<Permission> {
    let requested = &manifest.permissions;
    let mut out = Vec::new();
    for name in &requested.interfaces {
        out.push(permission("interface", name, interface(name)));
    }
    for entry in &requested.topics {
        out.push(permission("topic", entry, topic(entry)));
    }
    for entry in &requested.files {
        out.push(permission("file", entry, file(entry)));
    }
    for entry in &requested.network {
        out.push(permission("network", entry, network(entry)));
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
        assert_eq!(file("read:/data/home/*/pictures").risk, LOW);
        assert_eq!(file("write:/data/home/*/pictures").risk, MEDIUM);
        assert_eq!(file("read:/etc/passwd").risk, MEDIUM);
        assert_eq!(file("write:/data/apps/*").risk, HIGH);
        // A sibling that merely starts with the home path is not inside it.
        assert_eq!(file("write:/data/homework").risk, HIGH);
        assert_eq!(file("exec:/bin/sh").risk, HIGH);
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
             files = [\"read:/data/home/*/pictures\"]\nnetwork = [\"outbound\"]\n",
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
            ]
        );
        assert_eq!(listed[0].value, "os.lazy.clipboard.v1");
        assert_eq!(
            listed[0].explanation,
            "Read and change what you copy and paste"
        );
    }
}
