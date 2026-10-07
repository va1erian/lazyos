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
//! * **Develop** (`develop = true`, issue #529) allows spawning a child into
//!   any `dev:` label: the kernel scope `os.lazy.process.label.spawn.v1` with
//!   the wildcard method. The kernel still refuses a label `pkgd` has not
//!   loaded an approved rule set for ([`crate::develop`]).
//! * **Resident** (`[entry] resident = true`) adds the interfaces and topic
//!   [`crate::resident`] lists, compiled like the manifest's own entries.
//!
//! [`installed`] is what `pkgd` loads for an installed app: the manifest's
//! rules plus the [`baseline`] every app gets without asking, telling `init`
//! why it is failing (`os.lazy.init.v1` `ReportFailure` alone, issue #549).
//! That only labels the app's own failure notice, so it needs no consent.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_init_v1 as init;
use messenger_generated::os_lazy_messenger_names_resolve_v1 as resolve_scope;
use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;
use messenger_generated::os_lazy_messenger_topics_publish_v1 as publish_scope;
use messenger_generated::os_lazy_messenger_topics_subscribe_v1 as subscribe_scope;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_process_label_spawn_v1 as spawn_scope;

use crate::hash::{fnv1a32, fnv1a64};
use crate::resident;

/// Most rules the kernel keeps per label (`ipc::acl::MAX_LABEL_RULES`).
pub const MAX_RULES: usize = 256;
/// The wildcard method of a rule (`0xFFFFFFFF`).
pub const ANY_METHOD: u32 = u32::MAX;

/// The topics broker's interface and the name it is registered under.
const TOPICS_INTERFACE: &str = "os.lazy.messenger.topics.v1";
const NETWORK_INTERFACE: &str = "os.lazy.net.socket.v1";
/// `init`'s interface: every installed app may call its `ReportFailure`.
const INIT_INTERFACE: &str = "os.lazy.init.v1";

/// Service names that are not the interface name minus its `.vN`:
/// `(interface, extra service name)`.
pub const SERVICE_NAMES: &[(&str, &str)] = &[
    ("os.lazy.accounts.v1", "os.lazy.accountsd"),
    // `netd` serves the socket interface on the stack's name.
    ("os.lazy.net.socket.v1", "os.lazy.net.stack"),
    // `audiod` serves its control interface on the mixer's own name (the
    // Volume tray applet, docs/tray-plan.md T3).
    ("os.lazy.audio.mixer.v1", "os.lazy.audio"),
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

/// Policy scopes a service checks besides its own interface, by the interface
/// that implies them. A scope is a capability name the kernel authorizes like
/// an interface (`fnv1a64` of the name), not something an app calls, so it is
/// not in `idl/` and a manifest does not name it: asking for the clipboard is
/// asking to copy (`Offer`, the write scope) and paste (`Request`, the read
/// scope), which is what its explanation says ("Read and change what you copy
/// and paste"; `user/src/messenger/clipboard`).
const IMPLIED_SCOPES: &[(&str, &[&str])] = &[(
    "os.lazy.clipboard.v1",
    &["os.lazy.clipboard.write.v1", "os.lazy.clipboard.read.v1"],
)];

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
/// the broker's methods), network. With the [`baseline`] rules it does not
/// already hold, at most [`MAX_RULES`]: whatever compiles also installs.
pub fn compile(manifest: &Manifest) -> Result<Vec<LabelRule>, CompileError> {
    let requested = &manifest.permissions;
    let system_name = &manifest.app.system_name;
    let mut list = RuleList::default();
    // The manifest's own entries, then those `resident` implies: the same
    // lists the consent screen shows.
    for interface in resident::interfaces(manifest) {
        list.allow_interface(interface, None);
        let implied = IMPLIED_SCOPES.iter().filter(|(name, _)| *name == interface);
        for scope in implied.flat_map(|(_, scopes)| scopes.iter()) {
            list.allow(fnv1a64(scope), ANY_METHOD);
        }
    }
    let (mut publishes, mut subscribes) = (false, false);
    for entry in resident::topics(manifest) {
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
    if requested.develop {
        list.allow(spawn_scope::INTERFACE_ID, ANY_METHOD);
    }
    // Count the baseline `installed` adds, so the consent screen never
    // accepts a manifest that `pkgd` then fails to install.
    let baseline = baseline()
        .iter()
        .filter(|rule| !list.rules.contains(rule))
        .count();
    let needed = list.rules.len() + baseline;
    if needed > MAX_RULES {
        return Err(CompileError::TooManyRules { rules: needed });
    }
    Ok(list.rules)
}

/// The rules every installed app gets without asking: `init.ReportFailure`
/// and resolving `init`'s names.
pub fn baseline() -> Vec<LabelRule> {
    let mut list = RuleList::default();
    list.allow_interface(INIT_INTERFACE, Some(&[init::METHOD_REPORTFAILURE]));
    list.rules
}

/// What `pkgd` loads for an installed app: [`compile`], then the
/// [`baseline`] rules it does not already hold. At most [`MAX_RULES`].
pub fn installed(manifest: &Manifest) -> Result<Vec<LabelRule>, CompileError> {
    let mut rules = compile(manifest)?;
    for rule in baseline() {
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    if rules.len() > MAX_RULES {
        return Err(CompileError::TooManyRules { rules: rules.len() });
    }
    Ok(rules)
}

#[cfg(test)]
mod tests;
