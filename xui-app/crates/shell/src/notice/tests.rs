use super::*;

/// Every character is 7 pixels wide.
fn mono(text: &str) -> i32 {
    7 * text.chars().count() as i32
}

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
    assert_eq!(f.heading(), "Messenger stopped unexpectedly");
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
fn wrapping_keeps_every_word_within_the_width() {
    let lines = wrap("the quick brown fox jumps over the lazy dog", 70, &mono);
    assert!(lines.iter().all(|line| mono(line) <= 70), "{lines:?}");
    assert_eq!(
        lines.join(" "),
        "the quick brown fox jumps over the lazy dog"
    );
    let long = wrap("supercalifragilistic", 35, &mono);
    assert_eq!(long, ["super", "calif", "ragil", "istic"]);
    assert_eq!(wrap("", 70, &mono), [""]);
}

#[test]
fn the_layout_stacks_text_above_the_buttons() {
    let layout = Layout::new(&failure("main_form.rhai:3:7: boom", true), 18, &mono);
    assert_eq!(layout.width, WIDTH);
    assert!(layout.lines[0].heading);
    let last = layout.lines.last().expect("lines");
    assert!(last.text.starts_with("Reason:") || last.text.contains("boom"));
    assert!(layout.restart.y > last.y + 18 - 1);
    assert_eq!(layout.height, layout.close.y + BUTTON_H + PAD);
    assert!(layout.restart.x + layout.restart.w < layout.close.x);
    assert_eq!(layout.close.x + layout.close.w, WIDTH - PAD);
}

#[test]
fn buttons_are_hit_by_their_rectangles() {
    let layout = Layout::new(&failure("", false), 18, &mono);
    let r = layout.restart;
    let c = layout.close;
    assert_eq!(layout.hit(r.x + 1, r.y + 1), Some(Button::Restart));
    assert_eq!(
        layout.hit(c.x + c.w - 1, c.y + c.h - 1),
        Some(Button::Close)
    );
    assert_eq!(layout.hit(0, 0), None);
    assert_eq!(layout.button(Button::Close), c);
}

#[test]
fn a_long_reason_is_cut_with_an_ellipsis() {
    let reason = "word ".repeat(400);
    let layout = Layout::new(&failure(&reason, true), 18, &mono);
    let reason_lines: Vec<&Line> = layout
        .lines
        .iter()
        .filter(|line| !line.heading)
        .skip_while(|line| !line.text.starts_with("Reason:"))
        .collect();
    assert_eq!(reason_lines.len(), MAX_REASON_LINES);
    let last = reason_lines.last().expect("a reason line");
    assert!(last.text.ends_with("..."));
    assert!(mono(&last.text) <= WIDTH - 2 * PAD);
}
