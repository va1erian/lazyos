//! `pkg.install-devtool` (docs/devtool-packages-plan.md section 4, phase T3):
//! the operation of installing a developer-tool package, what its prompt
//! says and who may ask for it.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::approvals::{Approvals, Caller, LabelKind};
use crate::package::{self, Facts};
use crate::*;

fn parse(op: &str, items: &[&str]) -> Result<Operation, &'static str> {
    let args: Vec<String> = items.iter().map(|item| item.to_string()).collect();
    Operation::parse(op, &args)
}

fn facts(name: &str, system_name: &str, version: &str) -> Facts {
    Facts {
        name: String::from(name),
        system_name: String::from(system_name),
        version: String::from(version),
        author: String::from("Lazy Team"),
        permissions: Vec::new(),
        replaces: None,
    }
}

#[test]
fn the_operation_is_in_the_table_next_to_the_other_installs() {
    let position = |name: &str| NAMES.iter().position(|row| *row == name);
    assert_eq!(position("pkg.install"), Some(0));
    assert_eq!(position("pkg.update-core"), Some(1));
    assert_eq!(position("pkg.install-devtool"), Some(2));
    // `name()` and the table agree for every operation.
    let op = parse("pkg.install-devtool", &["/transient/playground.lzp"]).unwrap();
    assert_eq!(op.name(), "pkg.install-devtool");
    assert_eq!(
        op,
        Operation::PkgInstallDevtool {
            path: String::from("/transient/playground.lzp")
        }
    );
    // It is not one of the other two installs under another spelling.
    assert_ne!(
        Some(op.clone()),
        parse("pkg.install", &["/transient/playground.lzp"]).ok()
    );
}

#[test]
fn an_install_asks_every_time_and_no_view_approval_covers_it() {
    let op = parse("pkg.install-devtool", &["/transient/p.lzp"]).unwrap();
    assert_eq!(op.class(), Class::Once);
    assert!(op.permitted().is_ok());
    // The elevated editor's standing view approval (the only one that
    // stands) never covers it, for the same caller or any other.
    let caller = Caller {
        uid: 1000,
        label: 7,
        session: 1,
    };
    let mut approvals = Approvals::new();
    approvals.grant(caller, LabelKind::Other, Class::View, 0);
    assert!(approvals.covers(caller, LabelKind::Other, Class::View, 1));
    assert!(!approvals.covers(caller, LabelKind::Other, op.class(), 1));
    // And approving it leaves nothing standing.
    let mut after = Approvals::new();
    after.grant(caller, LabelKind::Other, op.class(), 0);
    assert_eq!(after.live(1), 0);
    assert!(!after.covers(caller, LabelKind::Other, Class::View, 1));
}

#[test]
fn only_an_unlabelled_program_asks_for_it() {
    let op = parse("pkg.install-devtool", &["/transient/p.lzp"]).unwrap();
    assert_eq!(op.asker_permitted(0), Ok(()));
    // An app, or a developer tool (which reaches `elevd` by default allow),
    // would hand its own authority on.
    for label in [1, 7, u32::MAX] {
        let why = op.asker_permitted(label).unwrap_err();
        assert!(why.contains("developer tool"), "{why}");
    }
    // The other operations are not judged by the label here.
    for (name, items) in [
        ("pkg.install", &["/transient/p.lzp"][..]),
        ("pkg.update-core", &["/transient/p.lzp"][..]),
        ("pkg.remove", &["org.lazy.demo"][..]),
        ("conf.elevate", &[][..]),
    ] {
        assert_eq!(parse(name, items).unwrap().asker_permitted(9), Ok(()));
    }
}

#[test]
fn the_prompt_leads_with_the_warning_and_names_the_package() {
    let text = package::tool_summary(&facts(
        "Messenger Playground",
        "os.lazy.playground",
        "1.0.0",
    ));
    assert!(text.starts_with(devtool::CONSENT_TEXT), "{text}");
    assert_eq!(
        text,
        alloc::format!(
            "{} Install \"Messenger Playground\" 1.0.0 (os.lazy.playground) for all users.",
            devtool::CONSENT_TEXT
        )
    );
    assert!(text.chars().count() <= MAX_SUMMARY, "{} chars", text.len());
    // The plain summary of the same package is not this one.
    let plain = package::summary(&facts(
        "Messenger Playground",
        "os.lazy.playground",
        "1.0.0",
    ));
    assert!(!plain.contains(devtool::CONSENT_TEXT));
}

#[test]
fn a_hostile_package_cannot_push_the_warning_out_or_overflow_the_prompt() {
    let huge = "W".repeat(500);
    let bidi = "Bob\u{202e}x\ny\"z\\";
    for (name, system_name, version) in [
        (huge.as_str(), huge.as_str(), huge.as_str()),
        (bidi, bidi, bidi),
        // Every character escapes to many: the worst case for the bound.
        (
            "\u{202e}\u{202e}\u{202e}\u{202e}\u{202e}\u{202e}",
            "\n\n\n\n\n\n",
            "\t\t\t\t\t\t",
        ),
    ] {
        let text = package::tool_summary(&facts(name, system_name, version));
        assert!(text.starts_with(devtool::CONSENT_TEXT), "{text}");
        assert!(
            text.chars().count() <= MAX_SUMMARY,
            "{} chars",
            text.chars().count()
        );
        assert!(text.ends_with(" for all users."), "{text}");
        assert!(
            text.chars()
                .all(|c| (' '..='\u{ff}').contains(&c) && c != '\u{7f}'),
            "{text}"
        );
        // A quote inside a name cannot end the quoted name early.
        assert_eq!(text.matches("Install \"").count(), 1);
    }
    // The room left after the warning is what the install line is given.
    let room = MAX_SUMMARY - devtool::CONSENT_TEXT.chars().count();
    assert!(room >= 82, "the warning left only {room} characters");
}

#[test]
fn the_prompt_says_the_tool_is_installed_for_all_users() {
    for (name, system_name, version) in [
        ("Messenger Playground", "os.lazy.playground", "1.0.0"),
        ("Paint", "org.lazy.paint", "1.2.0"),
    ] {
        let text = package::tool_summary(&facts(name, system_name, version));
        assert!(text.contains(" for all users"), "{text}");
        assert!(text.starts_with(devtool::CONSENT_TEXT), "{text}");
        assert!(text.chars().count() <= MAX_SUMMARY, "{text}");
    }
}

#[test]
fn the_author_is_named_when_it_fits_and_never_pushes_the_warning_out() {
    let short = package::tool_summary(&facts("Paint", "org.lazy.paint", "1.2.0"));
    assert_eq!(
        short,
        alloc::format!(
            "{} Install \"Paint\" 1.2.0 (org.lazy.paint) for all users; author \"Lazy Team\".",
            devtool::CONSENT_TEXT
        )
    );
    // A long author is elided to the room that is left.
    let mut long = facts("Paint", "org.lazy.paint", "1.2.0");
    long.author = "A".repeat(200);
    let text = package::tool_summary(&long);
    assert!(text.starts_with(devtool::CONSENT_TEXT), "{text}");
    assert!(text.contains("; author \""), "{text}");
    assert!(text.chars().count() <= MAX_SUMMARY, "{text}");
    // A hostile author cannot break the quote or the bound either.
    long.author = "\u{202e}\"\n\\".repeat(40);
    let text = package::tool_summary(&long);
    assert!(text.chars().count() <= MAX_SUMMARY, "{text}");
    assert!(text.ends_with("\"."), "{text}");
    assert_eq!(text.matches("; author \"").count(), 1);
}

#[test]
fn a_tool_prompt_is_never_a_plain_installs() {
    // The admin sees "Developer tool." first, whatever the path says.
    let op = parse("pkg.install-devtool", &["/transient/Developer tool.lzp"]).unwrap();
    assert!(op.summary().starts_with("Install the developer tool "));
    assert!(op.summary().chars().count() <= MAX_SUMMARY);
}

const TOOL_CALLER: Caller = Caller {
    uid: 1000,
    label: 12,
    session: 1,
};

#[test]
fn label_kinds_come_from_the_label_name() {
    assert_eq!(LabelKind::of(0, None), LabelKind::Unlabelled);
    assert_eq!(LabelKind::of(0, Some("tool:x")), LabelKind::Unlabelled);
    assert_eq!(
        LabelKind::of(12, Some("tool:os.lazy.playground")),
        LabelKind::Tool
    );
    assert_eq!(
        LabelKind::of(7, Some("app:os.lazy.settings")),
        LabelKind::Other
    );
    // `dev:` is an IDE run, not a tool; a bare prefix is not a tool label.
    assert_eq!(LabelKind::of(8, Some("dev:os.lazy.x")), LabelKind::Other);
    assert_eq!(LabelKind::of(9, Some("tool:")), LabelKind::Other);
    // A label that could not be named fails closed.
    assert_eq!(LabelKind::of(12, None), LabelKind::Unknown);
    assert!(LabelKind::Other.may_stand());
    for kind in [LabelKind::Unlabelled, LabelKind::Tool, LabelKind::Unknown] {
        assert!(!kind.may_stand(), "{kind:?}");
    }
}

#[test]
fn a_tool_label_is_granted_no_standing_view() {
    // Every script a tool runs shares its label: one approved read must not
    // give the next script admin-only reads without a prompt.
    for kind in [LabelKind::Tool, LabelKind::Unknown] {
        let mut approvals = Approvals::new();
        approvals.grant(TOOL_CALLER, kind, Class::View, 0);
        assert_eq!(approvals.live(1), 0, "{kind:?}");
        assert!(!approvals.covers(TOOL_CALLER, kind, Class::View, 1));
        assert!(!approvals.covers(TOOL_CALLER, LabelKind::Other, Class::View, 1));
    }
}

#[test]
fn an_existing_approval_is_not_honoured_for_a_tool_label() {
    let mut approvals = Approvals::new();
    approvals.grant(TOOL_CALLER, LabelKind::Other, Class::View, 0);
    assert!(approvals.covers(TOOL_CALLER, LabelKind::Other, Class::View, 1));
    // Whatever the table holds, a caller known to be a tool (or not named)
    // is asked again.
    assert!(!approvals.covers(TOOL_CALLER, LabelKind::Tool, Class::View, 1));
    assert!(!approvals.covers(TOOL_CALLER, LabelKind::Unknown, Class::View, 1));
}

#[test]
fn an_app_caller_keeps_its_standing_view() {
    let app = Caller {
        label: 7,
        ..TOOL_CALLER
    };
    let mut approvals = Approvals::new();
    approvals.grant(app, LabelKind::Other, Class::View, 0);
    assert!(approvals.covers(app, LabelKind::Other, Class::View, 1));
    assert!(!approvals.covers(app, LabelKind::Other, Class::Once, 1));
    // The tool's own label (another id) shares nothing with it.
    assert!(!approvals.covers(TOOL_CALLER, LabelKind::Other, Class::View, 1));
}

#[test]
fn updating_a_core_tool_says_update_and_keeps_the_warning_first() {
    let mut core = facts("Messenger Playground", "os.lazy.playground", "1.1.0");
    core.replaces = Some(String::from("1.0.0"));
    let text = package::tool_summary(&core);
    assert!(text.starts_with(devtool::CONSENT_TEXT), "{text}");
    assert!(
        text.contains(" Update \"Messenger Playground\" 1.1.0 (os.lazy.playground) for all users"),
        "{text}"
    );
    assert!(text.chars().count() <= MAX_SUMMARY);
}
