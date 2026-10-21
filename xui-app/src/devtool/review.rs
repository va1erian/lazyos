//! "Run a script you did not write here?": the review step for text that
//! arrived by paste, drop, file or share (docs/devtool-packages-plan.md
//! section 5, docs/messenger-playground-plan.md D7).
//!
//! [`ReviewGate`] is the pure part. A tool keeps one next to its editor,
//! tells it when text arrives ([`ReviewGate::new`], [`ReviewGate::arrive`])
//! and when the user edits ([`ReviewGate::edited`]), and asks it before every
//! Run ([`ReviewGate::needs_confirmation`]). [`review_dialog`] is the modal
//! card that asks, with Run and Cancel; the answer goes to
//! [`ReviewGate::confirm`] or is dropped. Nothing here runs anything: a Run is
//! always the user's own key or click.
//!
//! Rules, each tested:
//!
//! * Typed text and a Gallery entry (shipped inside the app, shown in the
//!   editor before it runs) are not asked about; clipboard, file and share
//!   text is.
//! * The question is asked once: after [`confirm`](ReviewGate::confirm) the
//!   buffer runs freely, edits included, until something else arrives.
//! * Text someone else wrote stays theirs through small edits. A pending
//!   buffer becomes the user's own only when less than half of the arrived
//!   text survives (what is left at both ends, unchanged), or when it was
//!   emptied. Pasting a script and adding a line is not writing it.
//! * What is judged is the *arrived span*, not the buffer. Pasting a script
//!   after text the user typed makes only the pasted span foreign; deleting
//!   the typed text around it leaves exactly the pasted script, which still
//!   asks. The gate follows the span through every edit (it diffs the buffer
//!   before and after) and finds it again by its text when an ambiguous diff
//!   slides it.
//! * A new arrival that replaces the whole buffer (a file opened) starts over
//!   with the whole buffer as the span; a paste into a buffer that still has
//!   an unconfirmed arrival in it widens the span to cover both.

use xui_core::app::Ui;
use xui_core::backend::Result;
use xui_core::widget::{TaskDialog, TaskDialogAction, TaskDialogIcon};

/// Where the text in a tool's buffer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provenance {
    /// The user typed it.
    Typed,
    /// Pasted, or imported from the clipboard.
    Clipboard,
    /// Opened from a file, or dropped onto the window.
    File,
    /// Loaded from the tool's own gallery of examples.
    Gallery,
    /// Received from another person or program (a shared link or document).
    Share,
}

impl Provenance {
    /// Whether text of this origin is asked about before its first Run.
    pub fn needs_review(self) -> bool {
        match self {
            Provenance::Typed | Provenance::Gallery => false,
            Provenance::Clipboard | Provenance::File | Provenance::Share => true,
        }
    }

    /// Where the dialog says the text came from.
    pub fn source_text(self) -> &'static str {
        match self {
            Provenance::Typed => "you typed",
            Provenance::Clipboard => "the clipboard",
            Provenance::File => "a file",
            Provenance::Gallery => "the gallery",
            Provenance::Share => "a share",
        }
    }
}

/// Where a gate stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Nothing to ask: the text is the user's own, or shipped with the app.
    Clear,
    /// Foreign text nobody has confirmed yet.
    Pending,
    /// The user confirmed.
    Confirmed,
}

/// Whether less than half of `baseline` is still at the two ends of `text`.
fn mostly_replaced(baseline: &[u8], text: &[u8]) -> bool {
    let prefix = baseline
        .iter()
        .zip(text)
        .take_while(|(a, b)| a == b)
        .count();
    // The suffix may not reach into the prefix, or one kept byte counts twice.
    let room = baseline.len().min(text.len()) - prefix;
    let suffix = baseline
        .iter()
        .rev()
        .zip(text.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix + suffix) * 2 < baseline.len()
}

/// The edit from `old` to `new` as `(start, removed, inserted)`: the bytes at
/// `start` in `old` that `inserted` bytes replaced in `new`, cut on character
/// boundaries.
fn region(old: &str, new: &str) -> (usize, usize, usize) {
    let (a, b) = (old.as_bytes(), new.as_bytes());
    let mut prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while prefix > 0 && !(old.is_char_boundary(prefix) && new.is_char_boundary(prefix)) {
        prefix -= 1;
    }
    let room = a.len().min(b.len()) - prefix;
    let mut suffix = a
        .iter()
        .rev()
        .zip(b.iter().rev())
        .take(room)
        .take_while(|(x, y)| x == y)
        .count();
    while suffix > 0
        && !(old.is_char_boundary(a.len() - suffix) && new.is_char_boundary(b.len() - suffix))
    {
        suffix -= 1;
    }
    (prefix, a.len() - suffix - prefix, b.len() - suffix - prefix)
}

/// Where the span `(start, end)` of the old text is after the edit that
/// replaced `removed` bytes at `at` by `inserted` bytes. An edit wholly
/// before the span (or ending where it starts) moves it, one wholly after it
/// (or starting where it ends) leaves it, and one that touches it grows or
/// shrinks it to take in everything the edit replaced.
fn follow(span: (usize, usize), at: usize, removed: usize, inserted: usize) -> (usize, usize) {
    let (start, end) = span;
    let shift = |x: usize| (x + inserted).saturating_sub(removed);
    if at + removed <= start {
        (shift(start), shift(end))
    } else if at >= end {
        span
    } else {
        (start.min(at), shift(end.max(at + removed)))
    }
}

/// The "confirm before the first Run" state of one editor buffer.
#[derive(Clone, Debug)]
pub struct ReviewGate {
    provenance: Provenance,
    /// The arrived text as it came, what an edit is measured against.
    baseline: String,
    /// Where that text sits in the buffer now, as a byte range.
    span: (usize, usize),
    /// The buffer as the gate last saw it, kept while it is pending: the
    /// "before" of the next edit.
    seen: String,
    state: State,
}

impl ReviewGate {
    /// A gate for `text` that arrived as `provenance`, all of it. Empty text
    /// has nothing to review.
    pub fn new(provenance: Provenance, text: &str) -> ReviewGate {
        ReviewGate::with_span(provenance, text, (0, text.len()))
    }

    fn with_span(provenance: Provenance, text: &str, span: (usize, usize)) -> ReviewGate {
        let (start, end) = (span.0.min(text.len()), span.1.min(text.len()));
        let pending = provenance.needs_review() && start < end;
        ReviewGate {
            provenance,
            baseline: text.get(start..end).unwrap_or_default().to_owned(),
            span: (start, end),
            seen: if pending {
                text.to_owned()
            } else {
                String::new()
            },
            state: if pending {
                State::Pending
            } else {
                State::Clear
            },
        }
    }

    /// New text replaced the buffer (a file opened, a share): `text` is the
    /// whole buffer now. Whatever was confirmed before no longer covers it.
    pub fn arrive(&mut self, provenance: Provenance, text: &str) {
        *self = ReviewGate::new(provenance, text);
    }

    /// Text arrived inside the buffer (a paste, a drop): the edit took `old`
    /// to `new`. Only what the edit inserted is the arrival, so text the user
    /// typed around it stays theirs and cannot be deleted to leave a script
    /// that was never asked about. An arrival that finds an unconfirmed one
    /// still in the buffer widens the span to cover both.
    pub fn arrive_edit(&mut self, provenance: Provenance, old: &str, new: &str) {
        let (at, removed, inserted) = region(old, new);
        let mut span = (at, at + inserted);
        if self.state == State::Pending {
            let (start, end) = follow(self.span, at, removed, inserted);
            span = (span.0.min(start), span.1.max(end));
        }
        *self = ReviewGate::with_span(provenance, new, span);
    }

    /// The origin of the text as the gate sees it now: the arrival's, or
    /// [`Provenance::Typed`] once the user has rewritten it.
    pub fn provenance(&self) -> Provenance {
        match self.state {
            State::Clear if self.provenance.needs_review() => Provenance::Typed,
            _ => self.provenance,
        }
    }

    /// Whether the next Run must ask first.
    pub fn needs_confirmation(&self) -> bool {
        self.state == State::Pending
    }

    /// The user answered Run: the buffer runs without asking again.
    pub fn confirm(&mut self) {
        if self.state == State::Pending {
            self.state = State::Confirmed;
            self.seen = String::new();
        }
    }

    /// The user edited the buffer; `text` is what it holds now. A pending
    /// arrival the user has mostly rewritten (or emptied) becomes theirs.
    pub fn edited(&mut self, text: &str) {
        if self.state != State::Pending {
            return;
        }
        let (at, removed, inserted) = region(&self.seen, text);
        let (start, end) = follow(self.span, at, removed, inserted);
        let (start, end) = (start.min(text.len()), end.min(text.len()));
        let window = &text.as_bytes()[start..end];
        if window == self.baseline.as_bytes() {
            self.span = (start, end);
        } else if let Some(found) = self.nearest(text, start) {
            // The arrival is still whole elsewhere (an edit around it was
            // ambiguous to diff): it keeps asking.
            self.span = (found, found + self.baseline.len());
        } else if mostly_replaced(self.baseline.as_bytes(), window) {
            self.state = State::Clear;
            self.seen = String::new();
            return;
        } else {
            self.span = (start, end);
        }
        self.seen = text.to_owned();
    }

    /// The occurrence of the arrived text in `text` nearest to `near`.
    fn nearest(&self, text: &str, near: usize) -> Option<usize> {
        if self.baseline.is_empty() {
            return None;
        }
        text.match_indices(self.baseline.as_str())
            .map(|(at, _)| at)
            .min_by_key(|at| at.abs_diff(near))
    }
}

/// How the review card was answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAnswer {
    /// Run it once; the gate is confirmed.
    Run,
    /// Do not run.
    Cancel,
}

/// The answer a dismissed card stands for: its only command is Run, and the
/// card's own Cancel button and Escape are Cancel.
pub fn answer_of(action: TaskDialogAction) -> ReviewAnswer {
    match action {
        TaskDialogAction::Command(0) => ReviewAnswer::Run,
        TaskDialogAction::Command(_) | TaskDialogAction::Cancel => ReviewAnswer::Cancel,
    }
}

/// What the card says under the question.
pub fn review_message(provenance: Provenance) -> String {
    format!(
        "This text came from {}, not from your typing. It runs with your full authority, \
         exactly like the Terminal. Read it first.",
        provenance.source_text()
    )
}

/// The modal card that asks [`devtool::REVIEW_PROMPT`]: Run, or Cancel. It
/// is built, opened and returned; keep it in the app (to ask
/// `TaskDialog::is_open`), and map the dismissal with `on_answer`:
///
/// ```ignore
/// let card = review_dialog(ui, gate.provenance(), |a| Some(Msg::Reviewed(a)))?;
/// // update: Msg::Reviewed(ReviewAnswer::Run) => { gate.confirm(); self.run(ui) }
/// ```
///
/// The card opens with the Run button focused, as every `TaskDialog` does;
/// Escape cancels.
pub fn review_dialog<M: 'static>(
    ui: &Ui<M>,
    provenance: Provenance,
    on_answer: impl Fn(ReviewAnswer) -> Option<M> + 'static,
) -> Result<TaskDialog<M>> {
    let dialog = TaskDialog::new(ui, devtool::REVIEW_PROMPT, &review_message(provenance))?
        .icon(TaskDialogIcon::Warning)
        .command("Run")?
        .on_action(move |action| on_answer(answer_of(action)));
    dialog.open();
    Ok(dialog)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "let a = 1;\nlet b = 2;\nprint(a + b);\nprint(\"done\");\n";

    #[test]
    fn only_text_that_was_not_typed_or_shipped_is_asked_about() {
        for (provenance, asks) in [
            (Provenance::Typed, false),
            (Provenance::Gallery, false),
            (Provenance::Clipboard, true),
            (Provenance::File, true),
            (Provenance::Share, true),
        ] {
            assert_eq!(provenance.needs_review(), asks, "{provenance:?}");
            let gate = ReviewGate::new(provenance, SCRIPT);
            assert_eq!(gate.needs_confirmation(), asks, "{provenance:?}");
        }
    }

    #[test]
    fn nothing_arrived_means_nothing_to_review() {
        assert!(!ReviewGate::new(Provenance::Clipboard, "").needs_confirmation());
    }

    #[test]
    fn it_asks_once() {
        let mut gate = ReviewGate::new(Provenance::File, SCRIPT);
        assert!(gate.needs_confirmation());
        gate.confirm();
        assert!(!gate.needs_confirmation());
        gate.edited("something else entirely");
        assert!(!gate.needs_confirmation(), "an edit does not re-arm it");
        assert_eq!(gate.provenance(), Provenance::File);
    }

    #[test]
    fn confirming_text_that_needs_no_review_changes_nothing() {
        let mut gate = ReviewGate::new(Provenance::Typed, SCRIPT);
        gate.confirm();
        assert!(!gate.needs_confirmation());
        assert_eq!(gate.provenance(), Provenance::Typed);
    }

    #[test]
    fn a_new_arrival_starts_over_even_after_a_confirmation() {
        let mut gate = ReviewGate::new(Provenance::File, SCRIPT);
        gate.confirm();
        gate.arrive(Provenance::Clipboard, "let x = 1;\n");
        assert!(gate.needs_confirmation());
        assert_eq!(gate.provenance(), Provenance::Clipboard);
        // A typed document replacing the buffer clears it.
        gate.arrive(Provenance::Typed, "print(1);");
        assert!(!gate.needs_confirmation());
    }

    #[test]
    fn a_small_edit_keeps_the_script_someone_elses() {
        let mut gate = ReviewGate::new(Provenance::Share, SCRIPT);
        gate.edited(&format!("{SCRIPT}print(\"one more\");\n"));
        assert!(gate.needs_confirmation(), "a line added at the end");
        // Edits arrive one at a time, as the editor reports them.
        gate.edited(SCRIPT);
        gate.edited(&SCRIPT.replace("let b = 2", "let b = 3"));
        assert!(gate.needs_confirmation(), "one character changed");
        gate.edited(&format!("// mine\n{SCRIPT}"));
        assert!(gate.needs_confirmation(), "a comment on top");
        assert_eq!(gate.provenance(), Provenance::Share);
    }

    #[test]
    fn a_rewritten_script_becomes_the_users_own() {
        let mut gate = ReviewGate::new(Provenance::Clipboard, SCRIPT);
        gate.edited("print(\"my own, quite different script\");\n");
        assert!(!gate.needs_confirmation());
        assert_eq!(gate.provenance(), Provenance::Typed);
        // And it stays the user's: putting the old text back does not re-arm.
        gate.edited(SCRIPT);
        assert!(!gate.needs_confirmation());
    }

    #[test]
    fn emptying_the_buffer_clears_it() {
        let mut gate = ReviewGate::new(Provenance::File, SCRIPT);
        gate.edited("");
        assert!(!gate.needs_confirmation());
    }

    #[test]
    fn the_materiality_threshold_is_half_of_the_arrived_bytes() {
        // 10 bytes arrived. Keeping 5 at the ends is still theirs, 4 is not.
        let mostly = |a: &str, b: &str| mostly_replaced(a.as_bytes(), b.as_bytes());
        assert!(!mostly("0123456789", "01234xxxxx"));
        assert!(mostly("0123456789", "0123xxxxxx"));
        assert!(!mostly("0123456789", "xxxxx56789"));
        // Both ends count: 3 + 2 kept bytes.
        assert!(!mostly("0123456789", "012xxxxx89"));
        // Kept bytes are never counted twice when the ends overlap.
        assert!(mostly("aaaaaaaaaa", "aaa"));
        assert!(!mostly("aaaaaaaaaa", "aaaaaa"));
        // Growing the text around the ends keeps them.
        assert!(!mostly("abcd", "abcdXXXXXXXXXXXX"));
        // Nothing left of anything.
        assert!(mostly("abcd", ""));
    }

    #[test]
    fn answers_map_to_run_and_cancel() {
        assert_eq!(answer_of(TaskDialogAction::Command(0)), ReviewAnswer::Run);
        assert_eq!(
            answer_of(TaskDialogAction::Command(1)),
            ReviewAnswer::Cancel
        );
        assert_eq!(answer_of(TaskDialogAction::Cancel), ReviewAnswer::Cancel);
    }

    #[test]
    fn the_message_names_the_source_and_the_authority() {
        for provenance in [Provenance::Clipboard, Provenance::File, Provenance::Share] {
            let message = review_message(provenance);
            assert!(message.contains(provenance.source_text()));
            assert!(message.contains("full authority"));
        }
    }
}

#[cfg(test)]
#[path = "review_span_tests.rs"]
mod span_tests;
