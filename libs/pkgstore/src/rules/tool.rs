//! The rules of a developer tool (`docs/devtool-packages-plan.md` section 3):
//! default allow, minus the reviewed deny list.
//!
//! The label `tool:<system_name>` is judged by the kernel's ordinary rule
//! engine, first match wins: one explicit deny for each interface of
//! [`devtool::DENY`] that applies to this manifest, then one allow of every
//! interface and method. `network = ["outbound"]` lifts the socket interface's
//! deny and `develop = true` the dev-spawn scope's, which is all those two
//! manifest fields mean for a tool. A shell tool (`shell = true`) also drops
//! the credential denies: it acts as the session's user and the services stay
//! the authority. Nothing else in the manifest compiles to
//! a rule: `interfaces` and `topics` must be empty (`lazypkg` refuses them),
//! `files` is consent-only as for an app, and the allow-all covers the
//! baseline and what `resident` implies, so neither is added.

use alloc::vec::Vec;

use devtool::{Deny, DenyWhen};
use lazypkg::Manifest;
use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;

use super::{CompileError, ANY_METHOD, MAX_RULES};
use crate::develop::ANY_INTERFACE;
use crate::hash::fnv1a64;

/// Whether `deny` is in force for `manifest`: always, or unless the manifest
/// declares the network or development power the entry guards.
pub fn applies(deny: &Deny, manifest: &Manifest) -> bool {
    let permissions = &manifest.permissions;
    match deny.when {
        DenyWhen::Always => true,
        DenyWhen::UnlessNetwork => !permissions.network.iter().any(|entry| entry == "outbound"),
        DenyWhen::UnlessDevelop => !permissions.develop,
        // `pkgd` honours `shell` only for a package the image ships
        // (`shellrole`): install refuses it elsewhere and boot reconciliation
        // refuses a row whose stored manifest claims it.
        DenyWhen::UnlessShell => !manifest.is_shell(),
    }
}

/// The deny-list entries in force for `manifest`, in list order. What the
/// consent screen names as refused and what [`compile`] denies come from this
/// one function.
pub fn denied(manifest: &Manifest) -> impl Iterator<Item = &'static Deny> + '_ {
    devtool::DENY
        .iter()
        .filter(move |deny| applies(deny, manifest))
}

/// The rules of `manifest` as a developer tool: the denies in force, then the
/// allow of everything.
pub fn compile(manifest: &Manifest) -> Result<Vec<LabelRule>, CompileError> {
    let mut rules: Vec<LabelRule> = denied(manifest)
        .map(|deny| LabelRule {
            interface_id: fnv1a64(deny.interface),
            method: ANY_METHOD,
            allow: false,
        })
        .collect();
    rules.push(LabelRule {
        interface_id: ANY_INTERFACE,
        method: ANY_METHOD,
        allow: true,
    });
    if rules.len() > MAX_RULES {
        return Err(CompileError::TooManyRules { rules: rules.len() });
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;

    fn tool(permissions: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Tool\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
             version = \"1.0.0\"\nkind = \"developer-tool\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n[permissions]\n{permissions}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    fn deny(name: &str) -> LabelRule {
        LabelRule {
            interface_id: fnv1a64(name),
            method: ANY_METHOD,
            allow: false,
        }
    }

    const ALLOW_ALL: LabelRule = LabelRule {
        interface_id: ANY_INTERFACE,
        method: ANY_METHOD,
        allow: true,
    };

    fn names(manifest: &Manifest) -> Vec<&'static str> {
        denied(manifest).map(|deny| deny.interface).collect()
    }

    #[test]
    fn a_plain_tool_denies_the_whole_list_then_allows_everything() {
        let rules = compile(&tool("")).unwrap();
        let expected: Vec<LabelRule> = devtool::DENY
            .iter()
            .map(|d| deny(d.interface))
            .chain([ALLOW_ALL])
            .collect();
        assert_eq!(rules, expected);
        assert_eq!(rules.len(), devtool::DENY.len() + 1);
        assert_eq!(rules.last(), Some(&ALLOW_ALL), "the allow comes last");
    }

    /// The Messenger doors to the network, derived from the deny list so a new
    /// `UnlessNetwork` entry is covered by the tests below without an edit
    /// here: the socket service, the stack (name lookups, ping) and `mountd`
    /// (FTP and SMB daemons). None needs a declaration beyond `network`, so a
    /// tool that did not declare it must not reach any (the consent text says
    /// "the network").
    fn network_denies() -> Vec<&'static str> {
        devtool::DENY
            .iter()
            .filter(|deny| deny.when == DenyWhen::UnlessNetwork)
            .map(|deny| deny.interface)
            .collect()
    }

    #[test]
    fn the_network_doors_are_the_three_the_consent_text_promises() {
        // Pinned by name once, so that deriving the list cannot silently
        // empty it (an empty list would make the tests below vacuous).
        assert_eq!(
            network_denies(),
            [
                "os.lazy.net.socket.v1",
                "os.lazy.net.stack.v1",
                "os.lazy.mount.v1"
            ]
        );
    }

    #[test]
    fn a_plain_tool_is_denied_every_door_to_the_network() {
        let rules = compile(&tool("")).unwrap();
        for name in network_denies() {
            assert!(rules.contains(&deny(name)), "{name}");
        }
    }

    #[test]
    fn outbound_lifts_the_three_network_denies_only() {
        let manifest = tool("network = [\"outbound\"]\n");
        let list = names(&manifest);
        for name in network_denies() {
            assert!(!list.contains(&name), "{name}");
        }
        assert!(list.contains(&"os.lazy.keyd.v1"));
        assert!(list.contains(&"os.lazy.process.label.spawn.v1"));
        let rules = compile(&manifest).unwrap();
        for name in network_denies() {
            assert!(!rules.contains(&deny(name)), "{name}");
        }
        assert_eq!(
            rules.len(),
            devtool::DENY.len() + 1 - network_denies().len()
        );
    }

    #[test]
    fn develop_lifts_the_dev_spawn_deny_only() {
        let manifest = tool("develop = true\n");
        let list = names(&manifest);
        assert!(!list.contains(&"os.lazy.process.label.spawn.v1"));
        for name in network_denies() {
            assert!(list.contains(&name), "{name}");
        }
        assert!(!compile(&manifest)
            .unwrap()
            .contains(&deny("os.lazy.process.label.spawn.v1")));
    }

    #[test]
    fn both_lifts_leave_only_the_always_and_credential_entries() {
        let manifest = tool("network = [\"outbound\"]\ndevelop = true\n");
        let always: Vec<&str> = devtool::DENY
            .iter()
            .filter(|d| matches!(d.when, DenyWhen::Always | DenyWhen::UnlessShell))
            .map(|d| d.interface)
            .collect();
        assert_eq!(names(&manifest), always);
        assert!(always.contains(&"os.lazy.keyd.v1"));
        assert!(always.contains(&"os.lazy.accounts.v1"));
    }

    #[test]
    fn the_credential_denies_hold_whatever_the_manifest_says() {
        let manifest =
            tool("network = [\"outbound\"]\ndevelop = true\nfiles = [\"read:$HOME/*\"]\n");
        for name in [
            "os.lazy.keyd.v1",
            "os.lazy.keyd.appsecret.v1",
            "os.lazy.accounts.v1",
            "os.lazy.messenger.policy.v1",
        ] {
            assert!(compile(&manifest).unwrap().contains(&deny(name)), "{name}");
        }
    }

    #[test]
    fn the_rule_counts_are_the_deny_list_plus_one_allow() {
        // Ten denies and the allow-all for a plain tool (the Playground's
        // `PKGD:POLICY:PASS ... rules=11`); `network` removes three, `develop`
        // one.
        assert_eq!(devtool::DENY.len(), 10);
        assert_eq!(compile(&tool("")).unwrap().len(), 11);
        assert_eq!(
            compile(&tool("network = [\"outbound\"]\n")).unwrap().len(),
            8
        );
        assert_eq!(compile(&tool("develop = true\n")).unwrap().len(), 10);
        assert_eq!(
            compile(&tool("network = [\"outbound\"]\ndevelop = true\n"))
                .unwrap()
                .len(),
            7
        );
    }

    #[test]
    fn a_deny_always_precedes_the_allow() {
        // First match wins in the kernel, so every deny must be ahead of the
        // wildcard allow or it would shadow nothing.
        for permissions in ["", "network = [\"outbound\"]\n", "develop = true\n"] {
            let rules = compile(&tool(permissions)).unwrap();
            let allow_at = rules.iter().position(|r| r.allow).unwrap();
            assert_eq!(allow_at, rules.len() - 1);
            assert!(rules[..allow_at].iter().all(|r| !r.allow));
            assert!(rules[..allow_at].iter().all(|r| r.method == ANY_METHOD));
        }
    }

    #[test]
    fn no_duplicates_and_far_below_the_kernels_limit() {
        let rules = compile(&tool("")).unwrap();
        for (index, rule) in rules.iter().enumerate() {
            assert!(!rules[..index].contains(rule), "duplicate {rule:?}");
        }
        assert!(rules.len() <= 16 && rules.len() < MAX_RULES);
    }

    #[test]
    fn every_deny_name_is_a_declared_interface_or_a_kernel_scope() {
        let manifest = include_str!("../../../../idl/manifest.json");
        for deny in devtool::DENY {
            let quoted = format!("\"interface\": \"{}\"", deny.interface);
            let declared = manifest.contains(&quoted);
            let scope = devtool::KERNEL_SCOPES.contains(&deny.interface);
            assert!(
                declared || scope,
                "{} is in devtool::DENY but neither in idl/ nor a kernel scope",
                deny.interface
            );
        }
    }

    #[test]
    fn the_dev_spawn_deny_matches_the_kernels_scope_id() {
        use messenger_generated::os_lazy_process_label_spawn_v1 as spawn_scope;
        let spawn = devtool::DENY
            .iter()
            .find(|d| d.when == DenyWhen::UnlessDevelop)
            .expect("the dev-spawn entry");
        assert_eq!(fnv1a64(spawn.interface), spawn_scope::INTERFACE_ID);
    }

    #[test]
    fn denied_names_are_ones_an_app_can_be_told_about() {
        // The explain table has a row for every denied service, so the
        // consent screen can say what it is.
        for deny in devtool::DENY {
            let row = crate::explain::interface(deny.interface);
            assert!(
                !row.text
                    .starts_with("An interface this system does not know"),
                "{}",
                deny.interface
            );
        }
    }
}
