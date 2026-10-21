use super::*;
use crate::installer::{Installed, Model, Request, Screen};
use crate::sys::errno;

fn row(kind: &str, value: &str, risk: &str, explanation: &str) -> Permission {
    Permission {
        kind: kind.into(),
        value: value.into(),
        risk: risk.into(),
        explanation: explanation.into(),
    }
}

/// The rows `pkgd` sends for a developer tool with one manifest permission
/// (`pkgstore::explain::tool`: kind, risks, denied, then the manifest's own).
fn tool_rows() -> Vec<Permission> {
    vec![
        row("kind", devtool::KIND, "high", devtool::CONSENT_TEXT),
        row(
            "risk",
            "Read the clipboard",
            "medium",
            "Read the clipboard: Sees what you last copied",
        ),
        row(
            "risk",
            "Flood the message fabric",
            "medium",
            "Flood the message fabric: A slow or stalled desktop",
        ),
        row(
            "denied",
            "os.lazy.keyd.v1",
            "low",
            "Cannot use: passwords and keys",
        ),
        row(
            "network",
            "outbound",
            "high",
            "Connects to the network",
        ),
    ]
}

fn tool() -> Package {
    Package {
        name: "Playground".into(),
        system_name: "org.lazy.playground".into(),
        version: "1.0.0".into(),
        permissions: tool_rows(),
        ..Package::default()
    }
}

fn plain() -> Package {
    Package {
        name: "Paint".into(),
        system_name: "org.lazy.paint".into(),
        permissions: vec![row("file", "read:$HOME/*", "high", "Reads your documents")],
        ..Package::default()
    }
}

#[test]
fn the_kind_row_alone_marks_a_developer_tool() {
    assert!(tool().is_developer_tool());
    assert!(!plain().is_developer_tool());
    assert!(!Package::default().is_developer_tool());
    // Risk and denied rows describe the class but do not make it.
    assert!(!is_developer_tool(&tool_rows()[1..]));
}

#[test]
fn a_look_alike_row_does_not_count() {
    // Same value under another kind, and the kind row with another value.
    assert!(!is_developer_tool(&[row(
        "interface",
        devtool::KIND,
        "high",
        ""
    )]));
    assert!(!is_developer_tool(&[row("kind", "application", "high", "")]));
    assert!(!is_developer_tool(&[row("kind", "Developer-Tool", "high", "")]));
}

#[test]
fn the_warning_is_the_class_row_text_or_the_shared_one() {
    assert_eq!(warning_text(&tool()), devtool::CONSENT_TEXT);
    let mut quiet = tool();
    quiet.permissions[0].explanation.clear();
    assert_eq!(warning_text(&quiet), devtool::CONSENT_TEXT);
    let mut own = tool();
    own.permissions[0].explanation = "Custom\nwords".into();
    assert_eq!(warning_text(&own), "Customwords", "control characters are stripped");
    assert_eq!(warning_text(&plain()), devtool::CONSENT_TEXT);
}

#[test]
fn the_tool_list_leads_with_the_packages_own_rows_then_the_class_and_drops_the_kind_row() {
    let items = tool_items(&tool());
    assert_eq!(items[0], "This package also asks for");
    assert_eq!(items[1], "  High risk");
    assert_eq!(items[2], "    Connects to the network (outbound)");
    assert_eq!(items[3], "What every developer tool can do");
    assert_eq!(items[4], "    Read the clipboard: Sees what you last copied");
    assert_eq!(items[5], "    Flood the message fabric: A slow or stalled desktop");
    assert_eq!(items[6], "What a developer tool may not use");
    assert_eq!(items[7], "    Cannot use: passwords and keys");
    assert_eq!(items.len(), 8);
    assert!(
        items.iter().all(|item| !item.contains("Developer tool.")),
        "the kind row leads the screen, not the list"
    );
}

#[test]
fn a_tool_with_no_own_permissions_lists_only_the_class() {
    let mut package = tool();
    package.permissions.truncate(4);
    let items = tool_items(&package);
    assert_eq!(items[0], "What every developer tool can do");
    assert!(items.iter().all(|item| !item.contains("also asks for")));
    // Without pkgd's class rows the list is one sentence, never empty.
    package.permissions.truncate(1);
    assert_eq!(
        tool_items(&package),
        vec!["This package requests no special permissions.".to_owned()]
    );
}

#[test]
fn class_rows_from_pkgd_are_elided_and_cleaned() {
    let mut package = tool();
    package.permissions[1].explanation = format!("x\u{1b}[2J{}", "y".repeat(400));
    let items = tool_items(&package);
    // Own rows first, then the heading, then the long row.
    assert_eq!(items[3], "What every developer tool can do");
    assert!(!items[4].contains('\u{1b}'));
    assert!(items[4].chars().count() <= 4 + 110);
    assert!(items[4].ends_with('…'));
}

#[test]
fn the_approval_note_names_the_tool_briefly() {
    let note = approval_note("Playground");
    assert_eq!(
        note,
        "Installing Playground needs an administrator's name and password."
    );
    assert!(approval_note(&"n".repeat(200)).chars().count() < 100);
}


#[test]
fn the_class_counts_are_the_risk_and_denied_rows() {
    assert_eq!(class_counts(&tool()), (2, 1));
    assert_eq!(class_counts(&plain()), (0, 0));
}

#[test]
fn a_tool_installs_through_its_own_operation_and_an_app_does_not() {
    assert_eq!(OPERATION, "pkg.install-devtool");
    let mut model = Model::new();
    model.inspect_ok("/transient/pg.lzp".into(), tool());
    assert!(model.advance());
    model.install_started();
    assert_eq!(
        model.pending,
        Some(Request::InstallTool("/transient/pg.lzp".into()))
    );

    let mut model = Model::new();
    model.inspect_ok("/transient/paint.lzp".into(), plain());
    assert!(model.advance());
    model.install_started();
    assert_eq!(
        model.pending,
        Some(Request::Install("/transient/paint.lzp".into()))
    );
}

#[test]
fn a_tool_that_would_replace_a_core_app_still_asks_for_the_tool_operation() {
    // The only door a tool has; `elevd` refuses the core replacement itself.
    let mut model = Model::new();
    model.list_loaded(vec![Installed {
        system_name: "org.lazy.playground".into(),
        core: true,
        ..Installed::default()
    }]);
    model.inspect_ok("/transient/pg.lzp".into(), tool());
    model.advance();
    model.install_started();
    assert!(matches!(model.pending, Some(Request::InstallTool(_))));
}

#[test]
fn the_review_and_consent_notes_name_the_class_and_a_replacement() {
    let mut model = Model::new();
    model.list_loaded(vec![Installed {
        system_name: "org.lazy.playground".into(),
        version: "0.9.0".into(),
        ..Installed::default()
    }]);
    model.inspect_ok("/transient/pg.lzp".into(), tool());
    let notes = model.consent_notes();
    assert_eq!(notes[0], "Developer tool: an administrator must approve it");
    assert_eq!(notes[1], "Replaces version 0.9.0 you have installed");

    let mut model = Model::new();
    model.inspect_ok("/transient/paint.lzp".into(), plain());
    assert!(model.consent_notes().is_empty(), "an app's notes are unchanged");
    assert_eq!(model.screen, Screen::Review);
}

#[test]
fn the_detail_names_the_installed_app() {
    assert_eq!(
        installed_name("Installed org.lazy.playground 1.0.0"),
        Some("org.lazy.playground")
    );
    assert_eq!(installed_name("Installed "), None);
    assert_eq!(installed_name("Removed x"), None);
    assert_eq!(installed_name(""), None);
}

#[test]
fn each_refusal_has_a_friendly_sentence() {
    let cancelled = refusal_text(-errno::ECANCELED, "the change was cancelled");
    assert!(cancelled.starts_with("Not installed: the administrator prompt was cancelled"));
    assert!(cancelled.contains("nothing was changed"));

    let nobody = refusal_text(-errno::EACCES, "no administrator approved the change");
    assert!(nobody.contains("ask an administrator"), "{nobody}");

    let late = refusal_text(-errno::ETIMEDOUT, "");
    assert!(late.contains("nobody answered"), "{late}");

    let not_a_tool = refusal_text(
        -errno::EINVAL,
        "that package is not a developer tool: install it with pkg.install",
    );
    assert!(not_a_tool.contains("is not a developer tool"), "{not_a_tool}");
    assert!(!not_a_tool.contains("pkg.install"), "no operation names for a person");

    let app = refusal_text(-errno::EPERM, "applications may not install developer tools");
    assert!(app.contains("can install a developer tool"), "{app}");
}

#[test]
fn an_unknown_refusal_shows_the_services_words_cleaned() {
    assert_eq!(
        refusal_text(-errno::EPERM, "that package would\nreplace a core app"),
        "Not installed: that package wouldreplace a core app"
    );
    let bare = refusal_text(-errno::EIO, "");
    assert!(bare.contains("errno 5"), "{bare}");
    assert!(bare.starts_with("Not installed:"));
}
