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
//! Rules, each tested (the gate itself is [`super::gate`]):
//!
//! * Typed text and a Gallery entry (shipped inside the app, shown in the
//!   editor before it runs) are not asked about; clipboard, file and share
//!   text is.
//! * The question is asked once: after [`confirm`](ReviewGate::confirm) the
//!   buffer runs freely, edits included, until something else arrives.
//! * Text someone else wrote stays theirs through edits: an arrival keeps
//!   asking while any 12 consecutive bytes of it are in the buffer, wherever
//!   they are, and each paste is its own arrival (typed text between two
//!   pastes is never swallowed into either). Pasting a script and adding a
//!   line is not writing it; so is deleting everything around its one
//!   dangerous line.
//! * What is judged is the arrived text, not the buffer: pasting a script
//!   after text the user typed makes only the pasted bytes foreign, and
//!   deleting the typed text around it leaves exactly the pasted script,
//!   which still asks.
//! * A new arrival that replaces the whole buffer (a file opened) starts over
//!   with the whole buffer as the arrival.

pub use super::gate::ReviewGate;
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
    fn the_materiality_threshold_is_a_preserved_run_of_twelve_bytes() {
        // An arrival keeps asking while 12 consecutive bytes of it are in the
        // buffer, wherever; 11 are not enough.
        let arrived = "0123456789abcdefghijklmnopqrstuvwxyz";
        for (kept, asks) in [(12, true), (11, false), (36, true), (1, false)] {
            let mut gate = ReviewGate::new(Provenance::Clipboard, arrived);
            gate.edited(&format!("typed {} typed", &arrived[36 - kept..]));
            assert_eq!(gate.needs_confirmation(), asks, "{kept} bytes kept");
        }
        // An arrival shorter than that is followed whole.
        let mut gate = ReviewGate::new(Provenance::Clipboard, "reboot();");
        gate.edited("// reboot();");
        assert!(gate.needs_confirmation());
        gate.edited("// reboot()");
        assert!(!gate.needs_confirmation());
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
