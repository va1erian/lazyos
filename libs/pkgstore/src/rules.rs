//! Compiling a manifest's permissions into the kernel's label rules.
//!
//! A task labelled `app:<system_name>` is default-deny
//! (`docs/architecture/ipc-security.md`): it may register `app.<system_name>.*`
//! services and use `app/<system_name>/` topics, and everything else needs an
//! allow rule loaded for its label. This module is the single place a manifest
//! turns into those rules; the installer's consent screen
//! ([`crate::explain`]) lists the same manifest entries, so what a person
//! approved is what the kernel enforces.
//!
//! * **Interfaces.** `interfaces = ["os.lazy.display.v1"]` allows every method
//!   of the interface and the *resolution* of the names it is served under (the
//!   kernel checks a labelled task's `Resolve` per exact name): the interface
//!   name itself, the name without its `.vN`, and the service name where it
//!   differs ([`SERVICE_NAMES`]). Unknown interfaces compile the same way: the
//!   consent screen already told the person it was unknown.
//! * **Topics.** The kernel authorizes a topic one *segment* at a time, so
//!   `subscribe:system/events/open/+` allows the segments `system`, `events`,
//!   `open` and `+` for subscribing. Segment grants combine: two topics that
//!   share segments also allow the paths in between. That is the kernel's
//!   granularity, not this compiler's; the namespace `app/<system_name>/` needs
//!   no rule. Any topic permission also needs the topics broker: its name, and
//!   the methods the direction uses.
//! * **Files** are not Messenger traffic. The kernel has no file sandbox yet,
//!   so `files` is consent-only today: recorded and shown, compiled to nothing.
//! * **Network** `outbound` allows the socket interface of the network stack.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_messenger_names_resolve_v1 as resolve_scope;
use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;
use messenger_generated::os_lazy_messenger_topics_publish_v1 as publish_scope;
use messenger_generated::os_lazy_messenger_topics_subscribe_v1 as subscribe_scope;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;

use crate::hash::{fnv1a32, fnv1a64};

/// Most rules the kernel keeps per label (`ipc::acl::MAX_LABEL_RULES`).
pub const MAX_RULES: usize = 256;
/// The wildcard method of a rule (`0xFFFFFFFF`).
pub const ANY_METHOD: u32 = u32::MAX;

/// The topics broker's interface and the name it is registered under.
const TOPICS_INTERFACE: &str = "os.lazy.messenger.topics.v1";
const NETWORK_INTERFACE: &str = "os.lazy.net.socket.v1";

/// Service names that are not the interface name minus its `.vN`:
/// `(interface, extra service name)`.
pub const SERVICE_NAMES: &[(&str, &str)] = &[
    ("os.lazy.accounts.v1", "os.lazy.accountsd"),
    // `netd` serves the socket interface on the stack's name.
    ("os.lazy.net.socket.v1", "os.lazy.net.stack"),
];

/// Why a manifest cannot be compiled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompileError {
    /// More rules than the kernel stores per label.
    TooManyRules { rules: usize },
}

impl core::fmt::Display for CompileError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CompileError::TooManyRules { rules } => write!(
                formatter,
                "the requested permissions need {rules} policy rules, more than the {MAX_RULES} the system keeps per app"
            ),
        }
    }
}

/// The policy label of an installed app.
pub fn label(system_name: &str) -> String {
    format!("app:{system_name}")
}

/// The names `interface` is served under, interface name first.
pub fn service_names(interface: &str) -> Vec<String> {
    let mut names = Vec::new();
    names.push(String::from(interface));
    if let Some((stem, version)) = interface.rsplit_once('.') {
        if version.starts_with('v') {
            names.push(String::from(stem));
        }
    }
    for (known, extra) in SERVICE_NAMES {
        if *known == interface {
            names.push(String::from(*extra));
        }
    }
    names
}

/// An ordered rule list without duplicates (a duplicate would only spend the
/// kernel's per-label budget).
#[derive(Default)]
struct RuleList {
    rules: Vec<LabelRule>,
}

impl RuleList {
    fn allow(&mut self, interface_id: u64, method: u32) {
        let rule = LabelRule {
            interface_id,
            method,
            allow: true,
        };
        if !self.rules.contains(&rule) {
            self.rules.push(rule);
        }
    }

    fn allow_resolve(&mut self, name: &str) {
        self.allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
    }

    /// Allow calling `methods` of `interface` (`None`: every method) and
    /// resolving every name it is served under.
    fn allow_interface(&mut self, interface: &str, methods: Option<&[u32]>) {
        let id = fnv1a64(interface);
        match methods {
            None => self.allow(id, ANY_METHOD),
            Some(methods) => methods.iter().for_each(|method| self.allow(id, *method)),
        }
        for name in service_names(interface) {
            self.allow_resolve(&name);
        }
    }
}

/// A `publish:`/`subscribe:` entry split into its direction and pattern.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Publish,
    Subscribe,
}

fn split_topic(entry: &str) -> Option<(Direction, &str)> {
    match entry.split_once(':')? {
        ("publish", pattern) => Some((Direction::Publish, pattern)),
        ("subscribe", pattern) => Some((Direction::Subscribe, pattern)),
        _ => None,
    }
}

/// Whether `pattern` lies in the app's own `app/<system_name>/` namespace,
/// which the kernel grants implicitly.
fn own_namespace(pattern: &str, system_name: &str) -> bool {
    let mut segments = pattern.split('/');
    segments.next() == Some("app") && segments.next() == Some(system_name)
}

/// The label rules for `manifest`, in a stable order: interfaces, topics (then
/// the broker's methods), network. At most [`MAX_RULES`].
pub fn compile(manifest: &Manifest) -> Result<Vec<LabelRule>, CompileError> {
    let requested = &manifest.permissions;
    let system_name = &manifest.app.system_name;
    let mut list = RuleList::default();
    for interface in &requested.interfaces {
        list.allow_interface(interface, None);
    }
    let (mut publishes, mut subscribes) = (false, false);
    for entry in &requested.topics {
        let Some((direction, pattern)) = split_topic(entry) else {
            continue;
        };
        match direction {
            Direction::Publish => publishes = true,
            Direction::Subscribe => subscribes = true,
        }
        if own_namespace(pattern, system_name) {
            continue;
        }
        let scope = match direction {
            Direction::Publish => publish_scope::INTERFACE_ID,
            Direction::Subscribe => subscribe_scope::INTERFACE_ID,
        };
        for segment in pattern.split('/') {
            list.allow(scope, fnv1a32(segment));
        }
    }
    if publishes || subscribes {
        // Reaching the broker is separate from being allowed a topic: the
        // broker's name and exactly the methods the direction needs.
        let mut methods = Vec::new();
        if publishes {
            methods.push(topics::METHOD_PUBLISH);
        }
        if subscribes {
            methods.extend([
                topics::METHOD_SUBSCRIBE,
                topics::METHOD_UNSUBSCRIBE,
                topics::METHOD_NEXTEVENT,
                topics::METHOD_ACK,
                topics::METHOD_STATS,
            ]);
        }
        list.allow_interface(TOPICS_INTERFACE, Some(&methods));
    }
    if requested.network.iter().any(|entry| entry == "outbound") {
        list.allow_interface(NETWORK_INTERFACE, None);
    }
    if list.rules.len() > MAX_RULES {
        return Err(CompileError::TooManyRules {
            rules: list.rules.len(),
        });
    }
    Ok(list.rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(permissions: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n[permissions]\n{permissions}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    fn allow(interface_id: u64, method: u32) -> LabelRule {
        LabelRule {
            interface_id,
            method,
            allow: true,
        }
    }

    #[test]
    fn the_label_is_app_colon_system_name() {
        assert_eq!(label("org.lazy.demo"), "app:org.lazy.demo");
    }

    #[test]
    fn no_permissions_compile_to_no_rules() {
        assert_eq!(compile(&manifest("")).unwrap(), Vec::new());
    }

    #[test]
    fn an_interface_allows_every_method_and_each_service_name() {
        let rules = compile(&manifest("interfaces = [\"os.lazy.display.v1\"]\n")).unwrap();
        let id = fnv1a64("os.lazy.display.v1");
        assert_eq!(
            rules,
            [
                allow(id, ANY_METHOD),
                allow(resolve_scope::INTERFACE_ID, fnv1a32("os.lazy.display.v1")),
                allow(resolve_scope::INTERFACE_ID, fnv1a32("os.lazy.display")),
            ]
        );
    }

    #[test]
    fn the_exact_rule_list_for_a_realistic_manifest() {
        let rules = compile(&manifest(
            "interfaces = [\"os.lazy.display.v1\", \"os.lazy.input.v1\"]\n\
             topics = [\"subscribe:system/events/open/+\", \"publish:app/org.lazy.demo/#\"]\n",
        ))
        .unwrap();
        let resolve = |name: &str| allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
        let sub = |segment: &str| allow(subscribe_scope::INTERFACE_ID, fnv1a32(segment));
        let topics_id = topics::INTERFACE_ID;
        let expected = [
            allow(fnv1a64("os.lazy.display.v1"), ANY_METHOD),
            resolve("os.lazy.display.v1"),
            resolve("os.lazy.display"),
            allow(fnv1a64("os.lazy.input.v1"), ANY_METHOD),
            resolve("os.lazy.input.v1"),
            resolve("os.lazy.input"),
            sub("system"),
            sub("events"),
            sub("open"),
            sub("+"),
            // The app's own namespace needs no segment rules, only the broker.
            allow(topics_id, topics::METHOD_PUBLISH),
            allow(topics_id, topics::METHOD_SUBSCRIBE),
            allow(topics_id, topics::METHOD_UNSUBSCRIBE),
            allow(topics_id, topics::METHOD_NEXTEVENT),
            allow(topics_id, topics::METHOD_ACK),
            allow(topics_id, topics::METHOD_STATS),
            resolve("os.lazy.messenger.topics.v1"),
            resolve("os.lazy.messenger.topics"),
        ];
        assert_eq!(rules, expected);
    }

    #[test]
    fn publish_only_does_not_get_the_subscribe_methods() {
        let rules = compile(&manifest("topics = [\"publish:app/org.lazy.demo/x\"]\n")).unwrap();
        let topics_id = topics::INTERFACE_ID;
        assert!(rules.contains(&allow(topics_id, topics::METHOD_PUBLISH)));
        assert!(!rules.contains(&allow(topics_id, topics::METHOD_SUBSCRIBE)));
        assert!(!rules.contains(&allow(topics_id, topics::METHOD_LISTTOPICS)));
        // Nothing but the broker: no segment rules for the own namespace.
        assert!(rules
            .iter()
            .all(|rule| rule.interface_id != publish_scope::INTERFACE_ID));
    }

    #[test]
    fn publishing_to_another_namespace_needs_segment_rules() {
        let rules = compile(&manifest(
            "topics = [\"publish:session/1/clipboard/changed\"]\n",
        ))
        .unwrap();
        let publish = |segment: &str| allow(publish_scope::INTERFACE_ID, fnv1a32(segment));
        for segment in ["session", "1", "clipboard", "changed"] {
            assert!(rules.contains(&publish(segment)), "{segment}");
        }
        assert!(!rules.contains(&allow(subscribe_scope::INTERFACE_ID, fnv1a32("session"))));
    }

    #[test]
    fn a_service_whose_name_differs_gets_both_names() {
        let rules = compile(&manifest("interfaces = [\"os.lazy.accounts.v1\"]\n")).unwrap();
        let resolve = |name: &str| allow(resolve_scope::INTERFACE_ID, fnv1a32(name));
        assert!(rules.contains(&resolve("os.lazy.accountsd")));
        assert!(rules.contains(&resolve("os.lazy.accounts")));
    }

    #[test]
    fn network_outbound_allows_the_socket_interface_and_files_compile_to_nothing() {
        let rules = compile(&manifest("network = [\"outbound\"]\n")).unwrap();
        assert!(rules.contains(&allow(fnv1a64("os.lazy.net.socket.v1"), ANY_METHOD)));
        assert!(rules.contains(&allow(
            resolve_scope::INTERFACE_ID,
            fnv1a32("os.lazy.net.stack")
        )));
        let files = compile(&manifest("files = [\"write:/home/*/x\"]\n")).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn every_rule_is_an_allow_and_there_are_no_duplicates() {
        let rules = compile(&manifest(
            "interfaces = [\"os.lazy.display.v1\", \"os.lazy.display.v1\", \"os.lazy.keyd.v1\"]\n\
             topics = [\"subscribe:system/events/+\", \"subscribe:system/events/open\"]\n",
        ))
        .unwrap();
        assert!(rules.iter().all(|rule| rule.allow));
        for (index, rule) in rules.iter().enumerate() {
            assert!(!rules[..index].contains(rule), "duplicate {rule:?}");
        }
    }

    #[test]
    fn too_many_rules_is_refused() {
        let interfaces: Vec<String> = (0..120).map(|index| format!("\"x.y{index}.v1\"")).collect();
        let text = format!("interfaces = [{}]\n", interfaces.join(", "));
        match compile(&manifest(&text)) {
            Err(CompileError::TooManyRules { rules }) => assert_eq!(rules, 360),
            other => panic!("expected TooManyRules, got {other:?}"),
        }
    }

    #[test]
    fn service_names_strip_the_version() {
        assert_eq!(
            service_names("os.lazy.confd.v1"),
            ["os.lazy.confd.v1", "os.lazy.confd"]
        );
        assert_eq!(service_names("weird"), ["weird"]);
        assert_eq!(
            service_names("os.lazy.net.socket.v1"),
            [
                "os.lazy.net.socket.v1",
                "os.lazy.net.socket",
                "os.lazy.net.stack"
            ]
        );
    }
}
