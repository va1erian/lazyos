//! Child labels: what a manifest's `[processes.<name>]` tables compile to
//! (issue #832, `docs/packages.md`).
//!
//! An app that starts a helper program names it in a process table: the
//! kernel gives an image of that program the label `app:<system_name>/<name>`
//! instead of the app's own, judged by the rules built here. They are the
//! app's rules for the same interfaces and network entry, nothing more:
//!
//! * no `topics` and no implicit namespace: a child owns no `app.<id>.*`
//!   service name and no `app/<id>/` topic (the kernel's `app_id_of` knows
//!   only `app:` and `dev:` labels), so none is compiled;
//! * no baseline (`init.ReportFailure`): the app reports for itself;
//! * `files` are enforced: they compile (`crate::filerules`) to the file gate's
//!   rule ids and the confine marker, which is what makes the label a sandbox.
//!
//! The kernel also checks that a child never holds more than the app: the
//! rules are refused unless the app's loaded rules cover each, and a call is
//! allowed only when the child's *and* the app's rules allow it
//! (`kernel/src/ipc/acl.rs`). The manifest check (`lazypkg`) is the early
//! refusal; this is the same set, compiled by the same functions as the
//! app's, so a manifest that passes it always loads.

use alloc::string::String;
use alloc::vec::Vec;

use lazypkg::Manifest;
use messenger_generated::os_lazy_messenger_policy_v1::LabelRule;

use super::{CompileError, RuleList, IMPLIED_SCOPES, MAX_RULES, NETWORK_INTERFACE};
use crate::hash::fnv1a64;

/// One declared child, ready to load.
#[derive(Clone, Debug, PartialEq)]
pub struct Child {
    /// The `[processes.<name>]` name.
    pub name: String,
    /// The kernel label, `app:<system_name>/<name>`.
    pub label: String,
    /// The program the kernel binds to the label: an `execve` of exactly this
    /// path by a task of the app's label takes it.
    pub program: String,
    /// The rules of the label. Possibly none: a page process asks for no
    /// Messenger interface at all, and the label is then default-deny.
    pub rules: Vec<LabelRule>,
}

/// The labels of the children `old` declared that `new` does not (every one
/// of them when `new` is `None`: the app was removed). `pkgd` revokes these
/// when an upgrade drops a process table and when a package goes, so a label
/// never outlives the manifest that named it. The kernel revokes a removed
/// app's children by itself as well (loading an empty rule list for an
/// `app:` label), which covers a manifest `pkgd` can no longer read.
pub fn dropped(old: &Manifest, new: Option<&Manifest>) -> Vec<String> {
    if old.is_developer_tool() {
        return Vec::new();
    }
    let system_name = &old.app.system_name;
    old.processes
        .keys()
        .filter(|name| new.is_none_or(|new| !new.processes.contains_key(*name)))
        .map(|name| lazypkg::child_label(system_name, name))
        .collect()
}

/// The child labels `manifest` declares, in name order, each with its
/// compiled rules.
pub fn children(manifest: &Manifest) -> Result<Vec<Child>, CompileError> {
    // A developer tool's children keep its own label (`lazypkg` refuses a
    // `[processes.<name>]` table beside the kind), so there is nothing to load.
    if manifest.is_developer_tool() {
        return Ok(Vec::new());
    }
    let system_name = &manifest.app.system_name;
    let mut out = Vec::with_capacity(manifest.processes.len());
    for (name, process) in &manifest.processes {
        let mut list = RuleList::default();
        for interface in &process.interfaces {
            list.allow_interface(interface, None);
            let implied = IMPLIED_SCOPES
                .iter()
                .filter(|(known, _)| *known == interface.as_str());
            for scope in implied.flat_map(|(_, scopes)| scopes.iter()) {
                list.allow(fnv1a64(scope), super::ANY_METHOD);
            }
        }
        if process.network.iter().any(|entry| entry == "outbound") {
            list.allow_interface(NETWORK_INTERFACE, None);
        }
        let files = crate::filerules::compile(&process.files).map_err(CompileError::Files)?;
        for rule in files {
            if !list.rules.contains(&rule) {
                list.rules.push(rule);
            }
        }
        if list.rules.len() > MAX_RULES {
            return Err(CompileError::TooManyRules {
                rules: list.rules.len(),
            });
        }
        out.push(Child {
            name: name.clone(),
            label: lazypkg::child_label(system_name, name),
            program: process.program.clone(),
            rules: list.rules,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::{compile, installed};
    use alloc::format;

    /// A manifest like the WebKit browser's: a window and a network, a page
    /// process with nothing and a network process with the network.
    fn webkit_like(tables: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Web\"\nsystem_name = \"org.lazy.web\"\nauthor = \"A\"\nversion = \"1.0.0\"\n\
             [entry]\nbinary = \"bin/app.elf\"\n\
             [permissions]\ninterfaces = [\"os.lazy.display.v1\", \"os.lazy.input.v1\"]\n\
             network = [\"outbound\"]\n\
             files = [\"read:/system/share/fonts\", \"write:$HOME/.apps/org.lazy.web\"]\n{tables}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    const TABLES: &str = "[processes.webprocess]\nprogram = \"/system/bin/webprocess\"\n\
                          files = [\"read:/system/share/fonts\"]\n\
                          [processes.networkprocess]\nprogram = \"/system/bin/networkprocess\"\n\
                          network = [\"outbound\"]\n\
                          files = [\"write:$HOME/.apps/org.lazy.web\"]\n";

    #[test]
    fn each_table_compiles_to_a_label_a_program_and_its_own_rules() {
        let manifest = webkit_like(TABLES);
        let kids = children(&manifest).unwrap();
        let names: Vec<&str> = kids.iter().map(|child| child.name.as_str()).collect();
        assert_eq!(names, ["networkprocess", "webprocess"]);
        let web = kids.iter().find(|c| c.name == "webprocess").unwrap();
        assert_eq!(web.label, "app:org.lazy.web/webprocess");
        assert_eq!(web.program, "/system/bin/webprocess");
        // A page process asks for no interface: only the file gate's rules,
        // the confine marker among them.
        let marker = filepolicy::confine_id();
        assert!(web.rules.iter().any(|rule| rule.interface_id == marker));
        let web_messenger: Vec<_> = web
            .rules
            .iter()
            .filter(|rule| rule.interface_id == fnv1a64(NETWORK_INTERFACE))
            .collect();
        assert!(web_messenger.is_empty());
        let net = kids.iter().find(|c| c.name == "networkprocess").unwrap();
        assert_eq!(net.label, "app:org.lazy.web/networkprocess");
        assert!(!net.rules.is_empty());
        // Exactly what the app's own `outbound` compiles to, and nothing of
        // the baseline.
        let net_only = compile(&webkit_like("")).unwrap();
        let files = crate::filerules::compile(&["write:$HOME/.apps/org.lazy.web"]).unwrap();
        for rule in &net.rules {
            assert!(net_only.contains(rule) || files.contains(rule), "{rule:?}");
        }
        assert!(net.rules.len() < net_only.len() + files.len());
    }

    #[test]
    fn a_childs_rules_are_a_subset_of_the_apps_installed_rules() {
        let manifest = webkit_like(
            "[processes.shell]\nprogram = \"/system/bin/helper\"\n\
             interfaces = [\"os.lazy.input.v1\"]\nnetwork = [\"outbound\"]\n",
        );
        let parent = installed(&manifest).unwrap();
        let kids = children(&manifest).unwrap();
        assert_eq!(kids.len(), 1);
        assert!(!kids[0].rules.is_empty());
        // Everything but the confine marker, which only the child carries.
        let marker = filepolicy::confine_id();
        assert!(kids[0].rules.iter().any(|rule| rule.interface_id == marker));
        for rule in kids[0].rules.iter().filter(|r| r.interface_id != marker) {
            assert!(parent.contains(rule), "the app lacks {rule:?}");
        }
        // The child does not get the app's other interface.
        let display = LabelRule {
            interface_id: fnv1a64("os.lazy.display.v1"),
            method: super::super::ANY_METHOD,
            allow: true,
        };
        assert!(parent.contains(&display));
        assert!(!kids[0].rules.contains(&display));
    }

    #[test]
    fn no_tables_no_children() {
        assert!(children(&webkit_like("")).unwrap().is_empty());
        assert!(dropped(&webkit_like(""), None).is_empty());
    }

    #[test]
    fn a_label_goes_with_its_table_or_its_package() {
        let old = webkit_like(TABLES);
        // Removing the package drops every child label.
        assert_eq!(
            dropped(&old, None),
            [
                "app:org.lazy.web/networkprocess",
                "app:org.lazy.web/webprocess"
            ]
        );
        // An upgrade that keeps one table drops the other.
        let kept = webkit_like("[processes.webprocess]\nprogram = \"/system/bin/webprocess\"\n");
        assert_eq!(
            dropped(&old, Some(&kept)),
            ["app:org.lazy.web/networkprocess"]
        );
        // One that keeps both drops none, and one with no tables drops both.
        assert!(dropped(&old, Some(&old)).is_empty());
        assert_eq!(dropped(&old, Some(&webkit_like(""))).len(), 2);
    }

    #[test]
    fn a_table_that_widens_the_app_never_reaches_the_compiler() {
        let text = "[app]\nname = \"W\"\nsystem_name = \"org.lazy.web\"\nauthor = \"A\"\n\
                    version = \"1.0.0\"\n[entry]\nbinary = \"bin/app.elf\"\n\
                    [permissions]\ninterfaces = [\"os.lazy.display.v1\"]\n\
                    [processes.web]\nprogram = \"/system/bin/web\"\n\
                    interfaces = [\"os.lazy.clipboard.v1\"]\n";
        let error = lazypkg::parse_manifest(text).expect_err("a wider child is refused");
        assert!(
            error.problems().iter().any(|p| p
                .message()
                .contains("is not in the app's permissions.interfaces")),
            "{error}"
        );
    }
}
