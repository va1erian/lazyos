//! Validation of `[app] kind` and of what a developer tool may declare
//! (`docs/devtool-packages-plan.md` section 2).
//!
//! A developer tool is default-allow minus a reviewed deny list, so the lists
//! that make an ordinary app default-deny have no meaning for it, and a list
//! that looked like a limit would be a lie the consent screen repeated. The
//! rest of the manifest keeps its meaning: `network` and `develop` lift deny
//! entries, `files` is shown at consent, `resident` is the tray behaviour.
//! `[processes.<name>]` is refused: the kernel has only `app:<id>/<name>` child
//! labels, and a tool's children inherit its label anyway.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Problem;
use crate::manifest::{Kind, Manifest};

/// Add a problem for everything wrong with the manifest's `kind` and, for a
/// developer tool, with what the kind does not allow.
pub(crate) fn check(manifest: &Manifest, problems: &mut Vec<Problem>) {
    let app = &manifest.app;
    if let Some(kind) = &app.kind {
        if Kind::parse(kind).is_none() {
            problems.push(Problem::new(format!(
                "app.kind {kind:?} must be {:?} or {:?}",
                Kind::APPLICATION,
                devtool::KIND
            )));
        }
    }
    if !manifest.is_developer_tool() {
        if app.shell.is_some() {
            problems.push(Problem::new(format!(
                "app.shell is only for a package with kind = {:?}",
                devtool::KIND
            )));
        }
        return;
    }
    let shell = manifest.is_shell();
    if app
        .category
        .as_deref()
        .is_some_and(|c| c != devtool::CATEGORY && !(shell && c == devtool::SHELL_CATEGORY))
    {
        problems.push(Problem::new(if shell {
            format!(
                "app.category of a shell {} must be {:?}, {:?} or absent",
                devtool::KIND,
                devtool::CATEGORY,
                devtool::SHELL_CATEGORY
            )
        } else {
            format!(
                "app.category of a {} must be {:?} or absent",
                devtool::KIND,
                devtool::CATEGORY
            )
        }));
    }
    let permissions = &manifest.permissions;
    for (field, empty) in [
        ("interfaces", permissions.interfaces.is_empty()),
        ("topics", permissions.topics.is_empty()),
    ] {
        if !empty {
            problems.push(Problem::new(format!(
                "permissions.{field} must be empty or absent for a {}: it can call anything \
                 but the reviewed deny list, so a list would only look like a limit",
                devtool::KIND
            )));
        }
    }
    if !manifest.processes.is_empty() {
        problems.push(Problem::new(String::from(
            "a developer-tool cannot declare [processes.<name>] tables: the programs it \
             starts keep its own label",
        )));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_manifest, validate};
    use alloc::collections::BTreeSet;
    use alloc::string::ToString;

    fn text(app: &str, rest: &str) -> String {
        format!(
            "[app]\nname = \"Tool\"\nsystem_name = \"org.lazy.tool\"\nauthor = \"A\"\n\
             version = \"1.0.0\"\n{app}\n[entry]\nbinary = \"bin/app.elf\"\n{rest}\n"
        )
    }

    fn problems(app: &str, rest: &str) -> Vec<String> {
        match parse_manifest(&text(app, rest)) {
            Ok(_) => Vec::new(),
            Err(error) => error
                .problems()
                .iter()
                .map(|p| p.message().to_string())
                .collect(),
        }
    }

    const TOOL: &str = "kind = \"developer-tool\"";

    #[test]
    fn an_absent_or_application_kind_is_an_application() {
        for app in ["", "kind = \"application\""] {
            let manifest = parse_manifest(&text(app, "")).expect("valid");
            assert_eq!(manifest.app.kind(), Kind::Application);
            assert!(!manifest.is_developer_tool());
        }
    }

    #[test]
    fn a_developer_tool_is_recognised() {
        let manifest = parse_manifest(&text(TOOL, "")).expect("valid");
        assert_eq!(manifest.app.kind(), Kind::DeveloperTool);
        assert!(manifest.is_developer_tool());
        // An absent category stays absent: the menu groups it as an app.
        assert_eq!(manifest.app.category, None);
    }

    #[test]
    fn an_unknown_kind_is_a_problem_and_not_a_tool() {
        for kind in ["tool", "Developer-Tool", "developer_tool", ""] {
            let app = format!("kind = {kind:?}");
            let found = problems(&app, "");
            assert_eq!(found.len(), 1, "{kind:?}: {found:?}");
            assert!(found[0].contains("app.kind"), "{found:?}");
            let manifest = crate::manifest::parse(&text(&app, "")).expect("parses");
            assert!(!manifest.is_developer_tool());
        }
    }

    #[test]
    fn the_kind_must_be_a_string() {
        assert!(crate::manifest::parse(&text("kind = 1", "")).is_err());
        assert!(crate::manifest::parse(&text("kind = true", "")).is_err());
    }

    #[test]
    fn a_tool_may_be_development_or_name_no_category() {
        assert!(problems(TOOL, "").is_empty());
        let app = format!("{TOOL}\ncategory = \"development\"");
        assert!(problems(&app, "").is_empty());
        let app = format!("{TOOL}\ncategory = \"games\"");
        let found = problems(&app, "");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("app.category"), "{found:?}");
        // An application keeps every category.
        assert!(problems("category = \"games\"", "").is_empty());
    }

    #[test]
    fn only_a_shell_may_be_listed_under_system() {
        let system = "category = \"system\"";
        let found = problems(
            &format!(
                "{TOOL}
{system}"
            ),
            "",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("app.category"), "{found:?}");
        assert!(problems(
            &format!(
                "{TOOL}
shell = true
{system}"
            ),
            ""
        )
        .is_empty());
        // The shell keeps the tools' own group too, and nothing else.
        assert!(problems(
            &format!(
                "{TOOL}
shell = true
category = \"development\""
            ),
            ""
        )
        .is_empty());
        let found = problems(
            &format!(
                "{TOOL}
shell = true
category = \"games\""
            ),
            "",
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("shell"), "{found:?}");
    }

    #[test]
    fn a_tool_lists_no_interfaces_or_topics() {
        let rest = "[permissions]\ninterfaces = [\"os.lazy.clipboard.v1\"]\n\
                    topics = [\"publish:app/org.lazy.tool/#\"]\n";
        let found = problems(TOOL, rest);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found[0].contains("permissions.interfaces"), "{found:?}");
        assert!(found[1].contains("permissions.topics"), "{found:?}");
        // The same lists are an application's business.
        assert!(problems("", rest).is_empty());
        // Empty lists are the same as absent ones.
        let empty = "[permissions]\ninterfaces = []\ntopics = []\n";
        assert!(problems(TOOL, empty).is_empty());
    }

    #[test]
    fn network_develop_files_and_resident_keep_their_meaning() {
        let rest = "resident = true\n[permissions]\nnetwork = [\"outbound\"]\ndevelop = true\n\
                    files = [\"read:$HOME/Documents/*\"]\n";
        let manifest = parse_manifest(&text(TOOL, rest)).expect("valid");
        assert!(manifest.entry.resident && manifest.permissions.develop);
        assert_eq!(manifest.permissions.network, ["outbound"]);
        // A bad value is still a problem for a tool.
        let bad = problems(TOOL, "[permissions]\nnetwork = [\"inbound\"]\n");
        assert_eq!(bad.len(), 1, "{bad:?}");
    }

    #[test]
    fn a_tool_declares_no_process_tables() {
        let rest = "[processes.helper]\nprogram = \"/system/bin/helper\"\n";
        let found = problems(TOOL, rest);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("[processes.<name>]"), "{found:?}");
        assert!(problems("", rest).is_empty());
    }

    #[test]
    fn archive_validation_applies_the_same_rules() {
        let manifest = crate::manifest::parse(&text(
            TOOL,
            "[permissions]\ninterfaces = [\"os.lazy.display.v1\"]\n",
        ))
        .expect("parses");
        let files: BTreeSet<&str> = ["bin/app.elf"].into_iter().collect();
        let found = validate::validate(&manifest, &files);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(validate::validate_standalone(&manifest).len() == 1);
    }

    #[test]
    fn shell_is_a_request_only_a_tool_can_make() {
        let shell = format!(
            "{TOOL}
shell = true"
        );
        let manifest = parse_manifest(&text(&shell, "")).expect("valid");
        assert!(manifest.is_shell() && manifest.is_developer_tool());
        // `shell = false` and an absent `shell` are not a request.
        let off = parse_manifest(&text(
            &format!(
                "{TOOL}
shell = false"
            ),
            "",
        ))
        .expect("valid");
        assert!(!off.is_shell());
        assert!(!parse_manifest(&text(TOOL, "")).expect("valid").is_shell());
        // An application cannot name it, true or false.
        for app in [
            "shell = true",
            "shell = false",
            "kind = \"application\"
shell = true",
        ] {
            let found = problems(app, "");
            assert_eq!(found.len(), 1, "{app:?}: {found:?}");
            assert!(found[0].contains("app.shell"), "{found:?}");
        }
        // And it must be a boolean.
        assert!(crate::manifest::parse(&text(
            &format!(
                "{TOOL}
shell = \"yes\""
            ),
            ""
        ))
        .is_err());
    }

    #[test]
    fn an_unknown_field_next_to_kind_is_still_refused() {
        assert!(crate::manifest::parse(&text(&format!("{TOOL}\nclass = \"x\""), "")).is_err());
    }
}
