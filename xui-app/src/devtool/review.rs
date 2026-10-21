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
//! * A new arrival (a paste into the buffer, a file opened) starts over with
//!   the whole buffer as the baseline.

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
fn mostly_replaced(baseline: &str, text: &str) -> bool {
    let (old, new) = (baseline.as_bytes(), text.as_bytes());
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    // The suffix may not reach into the prefix, or one kept byte counts twice.
    let room = old.len().min(new.len()) - prefix;
    let suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    (prefix + suffix) * 2 < old.len()
}

/// The "confirm before the first Run" state of one editor buffer.
#[derive(Clone, Debug)]
pub struct ReviewGate {
    provenance: Provenance,
    /// The buffer as it arrived, what an edit is measured against.
    baseline: String,
    state: State,
}

impl ReviewGate {
    /// A gate for `text` that arrived as `provenance`. Empty text has nothing
    /// to review.
    pub fn new(provenance: Provenance, text: &str) -> ReviewGate {
        let state = if provenance.needs_review() && !text.is_empty() {
            State::Pending
        } else {
            State::Clear
        };
        ReviewGate {
            provenance,
            baseline: text.to_owned(),
            state,
        }
    }

    /// New text arrived (a paste, a file opened, a share): `text` is the
    /// whole buffer now. Whatever was confirmed before no longer covers it.
    pub fn arrive(&mut self, provenance: Provenance, text: &str) {
        *self = ReviewGate::new(provenance, text);
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
        }
    }

    /// The user edited the buffer; `text` is what it holds now. A pending
    /// buffer the user has mostly rewritten (or emptied) becomes theirs.
    pub fn edited(&mut self, text: &str) {
        if self.state == State::Pending && mostly_replaced(&self.baseline, text) {
            self.state = State::Clear;
        }
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
        assert!(!mostly_replaced("0123456789", "01234xxxxx"));
        assert!(mostly_replaced("0123456789", "0123xxxxxx"));
        assert!(!mostly_replaced("0123456789", "xxxxx56789"));
        // Both ends count: 3 + 2 kept bytes.
        assert!(!mostly_replaced("0123456789", "012xxxxx89"));
        // Kept bytes are never counted twice when the ends overlap.
        assert!(mostly_replaced("aaaaaaaaaa", "aaa"));
        assert!(!mostly_replaced("aaaaaaaaaa", "aaaaaa"));
        // Growing the text around the ends keeps them.
        assert!(!mostly_replaced("abcd", "abcdXXXXXXXXXXXX"));
        // Nothing left of anything.
        assert!(mostly_replaced("abcd", ""));
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
