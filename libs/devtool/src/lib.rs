//! The developer-tool package class (`docs/devtool-packages-plan.md`).
//!
//! A developer tool is a package with `kind = "developer-tool"` whose calls are
//! chosen at run time by the user or by a script the user runs. It runs under
//! the label `tool:<system_name>`, whose Messenger rules are *default allow
//! minus a short deny list*. This crate is the one place that holds what the
//! pieces must agree on: the manifest spelling, the label prefix, the deny
//! list, and the texts shown on the consent screen, in the window and on the
//! Help page. It is `no_std`, allocation-free and data only, so the kernel
//! tests, `lazypkg`, `pkgstore` and the xui apps can all share it.

#![cfg_attr(not(test), no_std)]

/// The manifest spelling of the class (`[app] kind = "developer-tool"`).
pub const KIND: &str = "developer-tool";
/// The menu category a developer tool is listed under.
pub const CATEGORY: &str = "development";
/// The other menu category a developer tool that is the user's shell
/// (`[app] shell = true`, the Terminal) may name: the system's own group, where
/// the Terminal has always been listed beside Devices and the Installer.
pub const SHELL_CATEGORY: &str = "system";
/// The prefix of a developer tool's policy label (`tool:<system_name>`). Not
/// `dev:`, which already means "an app being run from an IDE".
pub const TOOL_PREFIX: &str = "tool:";

/// What is appended to a tool's window title.
pub const TITLE_SUFFIX: &str = " (developer tool)";
/// The strip every tool shows under its menu bar. The user may dismiss it (an "x" at
/// its right end; remembered per user and per tool, brought back by View > Show
/// developer-tool notice); that hides the band only, never the title suffix or the Help
/// page. The Installer's consent text is not this strip and is never dismissible.
pub const STRIP_TEXT: &str =
    "Runs with your full authority, exactly like the Terminal. Read a script before you run it.";
/// The question asked before the first run of text the user did not type.
pub const REVIEW_PROMPT: &str = "Run a script you did not write here?";

/// The first thing the consent screen and the Help page say.
pub const CONSENT_TEXT: &str = "Developer tool. Through the system's services it can do anything you can do, except use passwords, keys and your accounts, and open network connections unless listed. Its programs and scripts run with your permissions.";

/// One row of the class-wide risk table: what a tool can do and what follows.
/// What each app does about it is the app's own text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Risk {
    pub what: &'static str,
    pub consequence: &'static str,
}

/// The risks every developer tool shares, shown on the consent screen and on
/// each tool's Help page.
pub const RISKS: &[Risk] = &[
    Risk {
        what: "Stop or restart services, shut the system down",
        consequence: "Lost unsaved work, a broken session",
    },
    Risk {
        what: "Change or delete your own settings",
        consequence: "A broken terminal, theme or layout",
    },
    Risk {
        what: "Read the clipboard",
        consequence: "Sees what you last copied, perhaps a password",
    },
    Risk {
        what: "Launch applications and post notifications",
        consequence: "Look-alike notifications, surprise windows",
    },
    Risk {
        what: "Publish events other programs trust",
        consequence: "Spoofed events on your own session's topics",
    },
    Risk {
        what: "Flood the message fabric",
        consequence: "A slow or stalled desktop",
    },
    Risk {
        what: "Run a script someone else wrote",
        consequence: "Everything above, at once",
    },
];

/// When a deny-list entry applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyWhen {
    /// Always.
    Always,
    /// Unless the manifest declares `network = ["outbound"]`.
    UnlessNetwork,
    /// Unless the manifest declares `develop = true`.
    UnlessDevelop,
    /// Unless the tool is the user's shell (`[app] shell = true`, the
    /// Terminal): the credential services. A shell acts as the session's own
    /// user, as the unlabelled Terminal did, and the services stay the
    /// authority (they refuse everything the user may not do, and administrator
    /// actions go through `elevd`). Every other tool keeps the denial.
    UnlessShell,
}

/// One interface a developer tool may not call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deny {
    /// The interface name (`os.lazy.keyd.v1`); its id is `fnv1a64` of it.
    pub interface: &'static str,
    pub when: DenyWhen,
    pub reason: &'static str,
}

/// The deny list (a draft for review, `docs/devtool-packages-plan.md`
/// section 3): six entries, so a plain tool compiles to six denies and the
/// allow-all (seven rules); `network = ["outbound"]` and `develop = true`
/// remove one each, and a shell tool (`shell = true`) drops the three
/// credential entries (`UnlessShell`). Every name must exist in `idl/manifest.json` or be a kernel
/// scope listed in [`KERNEL_SCOPES`]; `pkgstore` tests check it.
pub const DENY: &[Deny] = &[
    Deny {
        interface: "os.lazy.keyd.v1",
        when: DenyWhen::UnlessShell,
        reason: "keys and secrets",
    },
    Deny {
        interface: "os.lazy.keyd.appsecret.v1",
        when: DenyWhen::UnlessShell,
        reason: "other programs' stored secrets",
    },
    Deny {
        interface: "os.lazy.accounts.v1",
        when: DenyWhen::UnlessShell,
        reason: "accounts and passwords",
    },
    Deny {
        interface: "os.lazy.messenger.policy.v1",
        when: DenyWhen::Always,
        // Loading a label's rules also needs a capability (`CAP_IPC_CONTROL`),
        // which no tool holds: this is defence in depth, so a mistaken grant
        // of the capability would not let a script rewrite its own rules.
        reason: "the label policy loader",
    },
    Deny {
        interface: "os.lazy.process.label.spawn.v1",
        when: DenyWhen::UnlessDevelop,
        reason: "running a child under another label",
    },
    Deny {
        interface: "os.lazy.net.socket.v1",
        when: DenyWhen::UnlessNetwork,
        reason: "network access must be declared",
    },
];

/// Interface names in [`DENY`] that are kernel ACL scopes: nothing serves
/// them (`idl/` declares them as scope-only interfaces, so they are in
/// `idl/manifest.json` too).
pub const KERNEL_SCOPES: &[&str] = &["os.lazy.process.label.spawn.v1"];

/// Whether `label` names a developer-tool label (`tool:<id>`).
pub fn is_tool_label(label: &str) -> bool {
    label
        .strip_prefix(TOOL_PREFIX)
        .is_some_and(|id| !id.is_empty())
}

/// The `system_name` of the tool whose label `label` is (`tool:os.lazy.x` ->
/// `os.lazy.x`); `None` for any other label.
pub fn tool_system_name(label: &str) -> Option<&str> {
    label.strip_prefix(TOOL_PREFIX).filter(|id| !id.is_empty())
}

/// The label of the tool `system_name`.
#[cfg(feature = "alloc")]
pub fn tool_label(system_name: &str) -> alloc::string::String {
    let mut label = alloc::string::String::from(TOOL_PREFIX);
    label.push_str(system_name);
    label
}

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_list_names_are_distinct_interfaces() {
        for (index, deny) in DENY.iter().enumerate() {
            assert!(deny.interface.starts_with("os.lazy."), "{}", deny.interface);
            assert!(!deny.reason.is_empty());
            assert!(DENY[..index].iter().all(|d| d.interface != deny.interface));
        }
    }

    #[test]
    fn kernel_scopes_are_in_the_deny_list() {
        for scope in KERNEL_SCOPES {
            assert!(DENY.iter().any(|d| d.interface == *scope), "{scope}");
        }
    }

    #[test]
    fn tool_labels_are_recognised() {
        assert!(is_tool_label("tool:os.lazy.playground"));
        assert!(!is_tool_label("tool:"));
        assert!(!is_tool_label("app:os.lazy.playground"));
        assert!(!is_tool_label("dev:os.lazy.playground"));
    }

    #[test]
    fn a_tool_label_names_its_package() {
        assert_eq!(
            tool_system_name("tool:os.lazy.terminal"),
            Some("os.lazy.terminal")
        );
        for other in ["tool:", "app:os.lazy.terminal", "terminal", ""] {
            assert_eq!(tool_system_name(other), None, "{other:?}");
        }
    }

    #[test]
    fn texts_are_not_empty() {
        assert!(!STRIP_TEXT.is_empty() && !CONSENT_TEXT.is_empty() && !REVIEW_PROMPT.is_empty());
        assert!(RISKS
            .iter()
            .all(|r| !r.what.is_empty() && !r.consequence.is_empty()));
    }
}
