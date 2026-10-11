use super::*;

fn failure(reason: &str, startup: bool) -> Failure {
    Failure {
        app: "user.me.messenger".into(),
        name: "Messenger".into(),
        summary: "exit code 2".into(),
        reason: reason.into(),
        startup,
    }
}

#[test]
fn the_text_names_the_app_and_says_what_happened() {
    let f = failure("boom", true);
    assert_eq!(f.title(), "Messenger stopped");
    assert!(f.sentence().contains("failed while starting (exit code 2)"));
    assert_eq!(f.reason_text().as_deref(), Some("Reason: boom"));
    let later = failure("  ", false);
    assert!(later
        .sentence()
        .contains("stopped with an error (exit code 2)"));
    assert_eq!(later.reason_text(), None);
}

#[test]
fn a_nameless_app_is_called_by_its_id() {
    let mut f = failure("", true);
    f.name.clear();
    assert_eq!(f.title(), "user.me.messenger stopped");
}

#[test]
fn the_toast_is_a_sticky_error_with_restart_and_close() {
    let request = failure("main_form.rhai:3:7: boom", true).request();
    let note = request.note;
    assert_eq!(note.level, Level::Error);
    assert_eq!(note.title, "Messenger stopped");
    assert!(note.body.contains("failed while starting"));
    assert!(note.body.ends_with("Reason: main_form.rhai:3:7: boom"));
    assert_eq!(
        note.actions
            .iter()
            .map(|a| (a.id, a.label.as_str()))
            .collect::<Vec<_>>(),
        [(ACTION_RESTART, "Restart"), (ACTION_CLOSE, "Close")]
    );
    assert_eq!(note.lifetime_ms(), None);
    assert_eq!(request.replaces, None);
}

#[test]
fn a_long_reason_is_cut_to_the_toasts_limit() {
    let reason = "word ".repeat(400);
    let note = failure(&reason, false).request().note;
    assert!(note.body.chars().count() <= BODY_MAX);
    assert!(note.body.ends_with("..."));
    let mut huge = failure("", true);
    huge.name = "N".repeat(500);
    assert!(huge.request().note.title.chars().count() <= TITLE_MAX);
}

#[test]
fn line_breaks_in_a_reason_become_spaces() {
    let note = failure(
        "line one
line two	tabbed",
        true,
    )
    .request()
    .note;
    assert!(note.body.ends_with("Reason: line one line two tabbed"));
}
