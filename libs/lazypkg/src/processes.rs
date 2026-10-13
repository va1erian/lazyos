//! Validation of the `[processes.<name>]` tables (issue #832, the child
//! labels of `docs/packages.md`).
//!
//! A process table names a helper program of the app and what it may do: the
//! kernel gives an image of that program the label `app:<system_name>/<name>`
//! instead of the app's own. The one rule that matters is that a table never
//! asks for more than the app itself holds: every interface must be one of
//! the app's `permissions.interfaces`, `outbound` needs the app's `outbound`,
//! and every files rule must be covered by one of the app's
//! ([`crate::files::covers`]). The kernel enforces the same containment when
//! the rules are loaded and again on every call (`kernel/src/ipc/acl.rs`), so
//! this check is the early, friendly refusal, not the only one.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::error::Problem;
use crate::files;
use crate::grammar::{valid_interface, valid_process_name, valid_program_path, MAX_PROCESS_NAME};
use crate::manifest::Manifest;

/// Most `[processes.<name>]` tables an app may declare. Each is a label in
/// the kernel's append-only table (256 for every app, service and
/// development run together).
pub const MAX_PROCESSES: usize = 4;

/// The label of the child `name` of `system_name`, as the kernel spells it.
pub fn child_label(system_name: &str, name: &str) -> String {
    format!("app:{system_name}/{name}")
}

/// Add a problem for everything wrong with the manifest's process tables.
pub(crate) fn check(manifest: &Manifest, problems: &mut Vec<Problem>) {
    if manifest.processes.len() > MAX_PROCESSES {
        problems.push(Problem::new(format!(
            "processes may hold at most {MAX_PROCESSES} tables"
        )));
    }
    let own = &manifest.permissions;
    let mut programs: BTreeSet<&str> = BTreeSet::new();
    for (name, process) in &manifest.processes {
        if !valid_process_name(name) {
            problems.push(Problem::new(format!(
                "processes name {name:?} must be 1..={MAX_PROCESS_NAME} characters of [a-z0-9_-], \
                 starting with a letter or digit"
            )));
            continue;
        }
        let field = |what: &str| format!("processes.{name}.{what}");
        let program = process.program.as_str();
        if !valid_program_path(program) {
            problems.push(Problem::new(format!(
                "{} {program:?} must be an absolute path of /-separated segments of [A-Za-z0-9_.-]",
                field("program")
            )));
        } else if !programs.insert(program) {
            problems.push(Problem::new(format!(
                "{} {program:?} is also the program of another process",
                field("program")
            )));
        }
        for interface in &process.interfaces {
            if !valid_interface(interface) {
                problems.push(Problem::new(format!(
                    "{} entry {interface:?} is not name.vN",
                    field("interfaces")
                )));
            } else if !own.interfaces.contains(interface) {
                problems.push(Problem::new(format!(
                    "{} entry {interface:?} is not in the app's permissions.interfaces",
                    field("interfaces")
                )));
            }
        }
        let outbound = process.network.iter().any(|entry| entry == "outbound");
        if !process.network.is_empty() && !(process.network.len() == 1 && outbound) {
            problems.push(Problem::new(format!(
                "{} must be empty or exactly [\"outbound\"]",
                field("network")
            )));
        } else if outbound && !own.network.iter().any(|entry| entry == "outbound") {
            problems.push(Problem::new(format!(
                "{} asks for outbound, which the app's permissions.network does not hold",
                field("network")
            )));
        }
        for rule in &process.files {
            if let Err(error) = files::check_rule(rule) {
                problems.push(Problem::new(error.message_in(&field("files"), rule)));
            } else if !own.files.iter().any(|held| files::covers(held, rule)) {
                problems.push(Problem::new(format!(
                    "{} entry {rule:?} is not covered by a permissions.files rule of the app \
                     (the same read:/write: and the same path or a directory above it)",
                    field("files")
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest;

    /// A manifest whose app holds `permissions` and which declares `tables`.
    fn manifest_of(permissions: &str, tables: &str) -> Manifest {
        let text = format!(
            "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\n\
             version = \"1.0.0\"\n[entry]\nbinary = \"bin/app.elf\"\n\
             [permissions]\n{permissions}\n{tables}\n"
        );
        manifest::parse(&text).expect("parses")
    }

    fn problems_of(permissions: &str, tables: &str) -> Vec<String> {
        let mut problems = Vec::new();
        check(&manifest_of(permissions, tables), &mut problems);
        problems
            .iter()
            .map(|problem| String::from(problem.message()))
            .collect()
    }

    const APP: &str = "interfaces = [\"os.lazy.display.v1\", \"os.lazy.input.v1\"]\n\
                       network = [\"outbound\"]\n\
                       files = [\"read:/system/share/fonts\", \"write:$HOME/.apps/org.lazy.demo\"]\n";

    #[test]
    fn no_tables_no_problems() {
        assert!(problems_of(APP, "").is_empty());
    }

    #[test]
    fn a_subset_is_accepted() {
        let tables = "[processes.web]\nprogram = \"/system/bin/webprocess\"\n\
                      [processes.net]\nprogram = \"/system/bin/networkprocess\"\n\
                      network = [\"outbound\"]\n\
                      interfaces = [\"os.lazy.input.v1\"]\n\
                      files = [\"read:/system/share/fonts/liberation\", \
                               \"write:$HOME/.apps/org.lazy.demo\"]\n";
        assert_eq!(problems_of(APP, tables), Vec::<String>::new());
        let m = manifest_of(APP, tables);
        assert_eq!(m.processes.len(), 2);
        assert_eq!(m.processes["web"].program, "/system/bin/webprocess");
        assert!(m.processes["web"].interfaces.is_empty());
        assert_eq!(child_label("org.lazy.demo", "web"), "app:org.lazy.demo/web");
    }

    #[test]
    fn a_table_that_asks_for_more_than_the_app_is_refused() {
        let cases = [
            (
                "interfaces = [\"os.lazy.clipboard.v1\"]",
                "is not in the app's permissions.interfaces",
            ),
            (
                "files = [\"read:$HOME\"]",
                "is not a read:/write: absolute or $HOME/ path",
            ),
            (
                "files = [\"read:$HOME/.ssh\"]",
                "is not covered by a permissions.files rule",
            ),
            (
                "files = [\"read:/system/share\"]",
                "is not covered by a permissions.files rule",
            ),
            (
                "files = [\"write:/system/share/fonts\"]",
                "is not covered by a permissions.files rule",
            ),
            (
                "files = [\"read:$HOME/.apps/org.lazy.demo\"]",
                "is not covered by a permissions.files rule",
            ),
            ("network = [\"inbound\"]", "must be empty or exactly"),
        ];
        for (field, wanted) in cases {
            let tables = format!("[processes.web]\nprogram = \"/system/bin/web\"\n{field}\n");
            let problems = problems_of(APP, &tables);
            assert_eq!(problems.len(), 1, "{field}: {problems:?}");
            assert!(problems[0].contains(wanted), "{field}: {problems:?}");
            assert!(problems[0].contains("processes.web."), "{problems:?}");
        }
    }

    #[test]
    fn outbound_needs_the_apps_outbound() {
        let tables = "[processes.web]\nprogram = \"/system/bin/web\"\nnetwork = [\"outbound\"]\n";
        let problems = problems_of("interfaces = []\n", tables);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("does not hold"), "{problems:?}");
    }

    #[test]
    fn names_programs_and_counts_are_bounded() {
        let bad_name = "[processes.Web]\nprogram = \"/system/bin/web\"\n";
        assert!(problems_of(APP, bad_name)[0].contains("processes name \"Web\""));
        for program in ["web", "/system/bin/../web", "/", ""] {
            let tables = format!("[processes.web]\nprogram = {program:?}\n");
            let problems = problems_of(APP, &tables);
            assert_eq!(problems.len(), 1, "{program:?}: {problems:?}");
            assert!(problems[0].contains("must be an absolute path"));
        }
        let twice = "[processes.a]\nprogram = \"/system/bin/x\"\n\
                     [processes.b]\nprogram = \"/system/bin/x\"\n";
        let problems = problems_of(APP, twice);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("also the program of another process"));
        let mut many = String::new();
        for index in 0..=MAX_PROCESSES {
            many += &format!("[processes.p{index}]\nprogram = \"/system/bin/p{index}\"\n");
        }
        let problems = problems_of(APP, &many);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("at most 4 tables"));
    }

    #[test]
    fn unknown_fields_and_a_missing_program_are_parse_errors() {
        for tables in [
            "[processes.web]\nprogram = \"/x\"\ntopics = [\"publish:a/b\"]\n",
            "[processes.web]\nprogram = \"/x\"\ndevelop = true\n",
            "[processes.web]\ninterfaces = []\n",
            "[processes]\nweb = \"/x\"\n",
        ] {
            let text = format!(
                "[app]\nname = \"D\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"A\"\n\
                 version = \"1.0.0\"\n[entry]\nbinary = \"bin/app.elf\"\n{tables}"
            );
            assert!(manifest::parse(&text).is_err(), "{tables}");
        }
    }
}
