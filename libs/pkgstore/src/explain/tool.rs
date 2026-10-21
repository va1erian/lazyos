//! What the consent screen says about a developer tool
//! (`docs/devtool-packages-plan.md` section 4).
//!
//! The rows come before the manifest's own, in this order: one `kind` row
//! carrying [`devtool::CONSENT_TEXT`], one `risk` row per entry of
//! [`devtool::RISKS`], then one `denied` row per deny-list entry in force for
//! the manifest ([`crate::rules::tool::denied`], the function that compiles
//! the denies, so the screen and the kernel agree). The texts are owned by the
//! `devtool` crate, which the Help page of every tool reads too.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use devtool::{Deny, DenyWhen};
use lazypkg::Manifest;
use messenger_generated::os_lazy_pkgd_v1::Permission;

use super::{permission, Explained, HIGH, LOW, MEDIUM};

/// The `kind` of the row that names the class.
pub const KIND_ROW: &str = "kind";
/// The value of the second `kind` row of a shell tool.
pub const SHELL_ROW_VALUE: &str = devtool::SHELL_ROW_VALUE;
/// The `kind` of a row that names a risk every developer tool shares.
pub const RISK_ROW: &str = "risk";
/// The `kind` of a row that names something the tool may not use.
pub const DENIED_ROW: &str = "denied";

/// Whether `row` describes the class rather than one request of the manifest
/// (so it does not count against the most permissions a package may request).
pub fn is_class_row(row: &Permission) -> bool {
    matches!(row.kind.as_str(), KIND_ROW | RISK_ROW | DENIED_ROW)
}

/// What a shell tool is, for the second `kind` row of its inspection.
pub const SHELL_TEXT: &str = "Shell tool: it is the user's shell, so the password, key and account services judge what it asks as they judge you.";

/// The sentence for one refused interface, in plain words.
pub fn denied_text(deny: &Deny) -> String {
    match deny.when {
        DenyWhen::Always => format!("Cannot use: {}", deny.reason),
        DenyWhen::UnlessNetwork | DenyWhen::UnlessDevelop => {
            format!("Cannot use unless the package declares it: {}", deny.reason)
        }
        DenyWhen::UnlessShell => {
            format!("Cannot use unless it is the user's shell: {}", deny.reason)
        }
    }
}

/// The class rows of `manifest`, which must be a developer tool.
pub fn rows(manifest: &Manifest) -> Vec<Permission> {
    let mut out = Vec::with_capacity(1 + devtool::RISKS.len() + devtool::DENY.len());
    let kind = Explained {
        risk: HIGH,
        // A shell tool is not refused the credential services, so the class
        // text that says it is would be false for it (`devtool::consent_text`).
        text: String::from(devtool::consent_text(manifest.is_shell())),
    };
    out.push(permission(KIND_ROW, devtool::KIND, kind));
    if manifest.is_shell() {
        // The administrator's prompt names a shell tool as one
        // (`elevpolicy::package::tool_summary`), and a shell is a different
        // thing to approve: it drops the credential denies below.
        let shell = Explained {
            risk: HIGH,
            text: String::from(SHELL_TEXT),
        };
        out.push(permission(KIND_ROW, SHELL_ROW_VALUE, shell));
    }
    for risk in devtool::RISKS {
        let explained = Explained {
            risk: MEDIUM,
            text: format!("{}: {}", risk.what, risk.consequence),
        };
        out.push(permission(RISK_ROW, risk.what, explained));
    }
    for deny in crate::rules::tool::denied(manifest) {
        let explained = Explained {
            risk: LOW,
            text: denied_text(deny),
        };
        out.push(permission(DENIED_ROW, deny.interface, explained));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::explain::permissions;

    fn manifest(app: &str, permissions: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Tool\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
             version = \"1.0.0\"\n{app}\n[entry]\nbinary = \"bin/app.elf\"\n\
             [permissions]\n{permissions}"
        );
        lazypkg::parse_manifest(&text).expect("valid")
    }

    fn tool(permissions: &str) -> Manifest {
        manifest("kind = \"developer-tool\"", permissions)
    }

    fn kinds(rows: &[Permission]) -> Vec<&str> {
        rows.iter().map(|row| row.kind.as_str()).collect()
    }

    #[test]
    fn a_tool_opens_with_its_kind_then_the_risks_then_the_denied_list() {
        let rows = permissions(&tool(""));
        assert_eq!(rows[0].kind, "kind");
        assert_eq!(rows[0].value, devtool::KIND);
        assert_eq!(rows[0].explanation, devtool::CONSENT_TEXT);
        assert_eq!(rows[0].risk, "high");
        let risks = &rows[1..1 + devtool::RISKS.len()];
        assert!(risks.iter().all(|row| row.kind == "risk"));
        for (row, risk) in risks.iter().zip(devtool::RISKS) {
            assert_eq!(row.value, risk.what);
            assert!(row.explanation.contains(risk.consequence), "{row:?}");
        }
        let denied = &rows[1 + devtool::RISKS.len()..];
        assert_eq!(denied.len(), devtool::DENY.len());
        for (row, deny) in denied.iter().zip(devtool::DENY) {
            assert_eq!(row.kind, "denied");
            assert_eq!(row.value, deny.interface);
            assert!(row.explanation.contains(deny.reason), "{row:?}");
        }
        assert_eq!(rows.len(), 1 + devtool::RISKS.len() + devtool::DENY.len());
    }

    #[test]
    fn a_shell_tool_says_so_in_a_second_kind_row_and_a_plain_one_does_not() {
        let plain = permissions(&tool(""));
        assert_eq!(
            plain.iter().filter(|row| row.kind == "kind").count(),
            1,
            "a plain tool has one kind row"
        );
        let shell = permissions(&manifest("kind = \"developer-tool\"\nshell = true", ""));
        let kinds_: Vec<&str> = shell
            .iter()
            .filter(|row| row.kind == "kind")
            .map(|row| row.value.as_str())
            .collect();
        assert_eq!(kinds_, [devtool::KIND, devtool::SHELL_ROW_VALUE]);
        assert!(shell.iter().filter(|row| is_class_row(row)).count() > 2);
        // The class row the Installer and the prompt lead with does not say the
        // credential services refuse a shell.
        assert_eq!(shell[0].explanation, devtool::SHELL_CONSENT_TEXT);
        assert_eq!(plain[0].explanation, devtool::CONSENT_TEXT);
        // The credential denies are gone for a shell, as the rules say.
        assert!(shell.iter().all(|row| row.value != "os.lazy.keyd.v1"));
    }

    #[test]
    fn declared_network_and_develop_leave_the_denied_list_and_show_as_their_own_rows() {
        let rows = permissions(&tool("network = [\"outbound\"]\ndevelop = true\n"));
        let denied: Vec<&str> = rows
            .iter()
            .filter(|row| row.kind == "denied")
            .map(|row| row.value.as_str())
            .collect();
        assert!(!denied.contains(&"os.lazy.net.socket.v1"));
        assert!(!denied.contains(&"os.lazy.net.stack.v1"));
        assert!(!denied.contains(&"os.lazy.mount.v1"));
        assert!(!denied.contains(&"os.lazy.process.label.spawn.v1"));
        assert!(denied.contains(&"os.lazy.keyd.v1"));
        // The manifest's own rows follow the class rows.
        let tail = kinds(&rows[rows.len() - 2..]);
        assert_eq!(tail, ["network", "develop"]);
        let first_own = rows.iter().position(|row| !is_class_row(row)).unwrap();
        assert!(rows[..first_own].iter().all(is_class_row));
    }

    #[test]
    fn the_conditional_entries_say_they_can_be_declared() {
        let rows = permissions(&tool(""));
        let socket = rows
            .iter()
            .find(|row| row.value == "os.lazy.net.socket.v1")
            .unwrap();
        assert!(
            socket.explanation.starts_with("Cannot use unless"),
            "{socket:?}"
        );
        let keyd = rows
            .iter()
            .find(|row| row.value == "os.lazy.keyd.v1")
            .unwrap();
        assert!(
            keyd.explanation
                .starts_with("Cannot use unless it is the user's shell"),
            "{keyd:?}"
        );
        let policy = rows
            .iter()
            .find(|row| row.value == "os.lazy.messenger.policy.v1")
            .unwrap();
        assert!(policy.explanation.starts_with("Cannot use:"), "{policy:?}");
    }

    #[test]
    fn an_application_gets_no_class_rows() {
        let rows = permissions(&manifest("", "network = [\"outbound\"]\n"));
        assert_eq!(kinds(&rows), ["network"]);
        assert!(!rows.iter().any(is_class_row));
        let rows = permissions(&manifest("kind = \"application\"", ""));
        assert!(rows.is_empty());
    }

    #[test]
    fn every_row_has_a_risk_word_and_a_sentence() {
        for row in permissions(&tool("develop = true\nnetwork = [\"outbound\"]\n")) {
            let risk = row.risk.as_str();
            assert!(["low", "medium", "high"].contains(&risk), "{row:?}");
            assert!(!row.explanation.is_empty() && !row.value.is_empty());
        }
    }

    #[test]
    fn a_resident_tool_lists_only_the_resident_row_of_its_own() {
        let text = "[app]\nname = \"T\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
                    version = \"1.0.0\"\nkind = \"developer-tool\"\n\
                    [entry]\nbinary = \"bin/app.elf\"\nresident = true\n";
        let rows = permissions(&lazypkg::parse_manifest(text).unwrap());
        // `resident` implies interfaces for an app; a tool already holds
        // everything, so only the resident row itself is listed.
        let own: Vec<&str> = rows
            .iter()
            .filter(|row| !is_class_row(row))
            .map(|row| row.kind.as_str())
            .collect();
        assert_eq!(own, ["resident"]);
    }
}
