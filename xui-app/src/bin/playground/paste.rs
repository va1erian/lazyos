//! Text that arrives in the editor, and the review gate (D7).
//!
//! Two things tell the window an edit was not typing. The editor's clipboard
//! is wrapped in a [`TappedClipboard`]: Ctrl+V and Edit > Paste both read it,
//! so a read since the last edit marks the next edit as a paste, however
//! short the pasted text is. And [`xui_playground::arrival`] judges the edit
//! from the text before and after it: what it *inserted* (not how much the
//! buffer grew) is more than a key can type, which covers a drop or any
//! other way text gets in. [`apply_edit`] feeds the answer to the gate.

use std::cell::Cell;
use std::rc::Rc;

use xui_app::devtool::{Provenance, ReviewGate};
use xui_code_editor::Clipboard;
use xui_playground::arrival::{classify, Kind, Known};

/// Whether the editor read the clipboard since the window last looked.
#[derive(Clone, Default)]
pub struct PasteTap(Rc<Cell<bool>>);

impl PasteTap {
    /// Whether a paste ran since the last call; clears it.
    pub fn take(&self) -> bool {
        self.0.replace(false)
    }

    fn mark(&self) {
        self.0.set(true);
    }
}

/// The editor's clipboard with a tap on its reads. The editor reads the
/// clipboard only to paste (`Editor::can_paste`, which also reads it, is not
/// used by this window).
pub struct TappedClipboard {
    inner: Box<dyn Clipboard>,
    tap: PasteTap,
}

impl TappedClipboard {
    pub fn new(inner: Box<dyn Clipboard>, tap: PasteTap) -> TappedClipboard {
        TappedClipboard { inner, tap }
    }
}

impl Clipboard for TappedClipboard {
    fn text(&self) -> Option<String> {
        let text = self.inner.text();
        if text.as_deref().is_some_and(|t| !t.is_empty()) {
            self.tap.mark();
        }
        text
    }

    fn set_text(&self, text: &str) {
        self.inner.set_text(text);
    }
}

/// Tell `gate` about the edit from `old` to `new`. An arrival (a paste, or
/// more than a key inserts) makes what the edit inserted an arrival under
/// review (the user's own text around it stays theirs, but deleting it leaves
/// the arrival asking); typing leaves it to the gate; a text the gate had vouched for
/// (Undo, Redo) is the user's again. Whatever the gate lets run as it stands
/// is remembered in `known`.
pub fn apply_edit(
    gate: &mut ReviewGate,
    known: &mut Known,
    old: &str,
    new: &str,
    read_clipboard: bool,
) {
    match classify(old, new, read_clipboard, known) {
        Kind::Unchanged => return,
        Kind::Arrival => gate.arrive_edit(Provenance::Clipboard, old, new),
        Kind::Typing => gate.edited(new),
        Kind::Restore => {
            if gate.needs_confirmation() {
                // A pending paste was undone: this exact text was the user's.
                gate.arrive(Provenance::Typed, new);
            } else {
                gate.edited(new);
            }
        }
    }
    vouch(gate, known, new);
}

/// Remember `text` if the gate lets it run without asking.
pub fn vouch(gate: &ReviewGate, known: &mut Known, text: &str) {
    if !gate.needs_confirmation() {
        known.remember(text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const LONG: &str = "let a = 1;\nlet b = 2;\nprint(a + b);\nprint(\"done\");\n";
    const HOSTILE: &str = "sys::init::shutdown();";

    /// A buffer the way the window keeps it.
    struct Buffer {
        text: String,
        gate: ReviewGate,
        known: Known,
    }

    impl Buffer {
        fn typed(text: &str) -> Buffer {
            let gate = ReviewGate::new(Provenance::Typed, text);
            let mut known = Known::default();
            vouch(&gate, &mut known, text);
            Buffer {
                text: text.to_string(),
                gate,
                known,
            }
        }

        /// The editor reported `text`; `paste` is whether it read the clipboard.
        fn edit(&mut self, text: &str, paste: bool) {
            apply_edit(&mut self.gate, &mut self.known, &self.text, text, paste);
            self.text = text.to_string();
        }

        fn type_str(&mut self, typed: &str) {
            for c in typed.chars() {
                let next = format!("{}{c}", self.text);
                self.edit(&next, false);
            }
        }

        fn confirm(&mut self) {
            self.gate.confirm();
            vouch(&self.gate, &mut self.known, &self.text);
        }

        fn asks(&self) -> bool {
            self.gate.needs_confirmation()
        }
    }

    #[test]
    fn select_all_and_paste_a_shorter_script_asks() {
        let mut b = Buffer::typed(LONG);
        assert!(!b.asks());
        // Ctrl+A, Ctrl+V: the buffer shrinks, the old rule saw typing.
        b.edit(HOSTILE, true);
        assert!(b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Clipboard);
        // The same without the tap (a drop, or a paste path the tap misses).
        let mut b = Buffer::typed(LONG);
        b.edit(HOSTILE, false);
        assert!(b.asks());
    }

    #[test]
    fn pasting_over_a_selection_asks() {
        let mut b = Buffer::typed("let a = 1;\nprint(a);\n");
        b.edit("let a = 1;\nevil();\n", true);
        assert!(b.asks());
        let mut b = Buffer::typed("let a = 1;\nprint(a);\n");
        b.edit("let a = 1;\nevil();\n", false);
        assert!(b.asks(), "more than a key inserted, tap or not");
    }

    #[test]
    fn many_small_pastes_into_a_confirmed_buffer_ask_again() {
        let mut b = Buffer::typed("");
        b.edit(LONG, true);
        assert!(b.asks());
        b.confirm();
        assert!(!b.asks());
        // One character per paste: the text alone cannot tell it from a key.
        for c in ["s", "y", "s"] {
            let next = format!("{}{c}", b.text);
            b.edit(&next, true);
            assert!(b.asks(), "paste of {c:?}");
            b.confirm();
        }
        // Two characters per paste are caught without the tap as well.
        let next = format!("{}ab", b.text);
        b.edit(&next, false);
        assert!(b.asks());
    }

    #[test]
    fn typing_never_asks_whatever_the_buffer_holds() {
        let mut b = Buffer::typed("");
        b.type_str("let x = [1, 2];\nfor i in x {\n");
        // Enter copies the indentation: a line break and spaces at once.
        b.edit(&format!("{}\n    ", b.text), false);
        b.type_str("print(i);");
        b.edit(&format!("{}\t\t", b.text), false);
        // A selection replaced by one character, a backspace, a delete-all.
        let replaced = b.text.replacen('x', "y", 1);
        b.edit(&replaced, false);
        let shorter = b.text[..b.text.len() - 1].to_string();
        b.edit(&shorter, false);
        b.edit("", false);
        assert!(!b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Typed);
        // And in a buffer the user confirmed, typing keeps it confirmed.
        let mut b = Buffer::typed("");
        b.edit(LONG, true);
        b.confirm();
        b.type_str("print(1);");
        assert!(!b.asks());
    }

    #[test]
    fn a_paste_into_a_typed_buffer_is_foreign_until_confirmed() {
        let mut b = Buffer::typed("print(1);\n");
        let pasted = format!("{}{HOSTILE}\n", b.text);
        b.edit(&pasted, true);
        assert!(b.asks());
        b.confirm();
        assert!(!b.asks());
        // The confirmed buffer is edited freely.
        let edited = pasted.replace("print(1)", "print(2)");
        b.edit(&edited, false);
        assert!(!b.asks());
    }

    #[test]
    fn undo_of_an_edit_in_a_confirmed_buffer_stays_confirmed() {
        let mut b = Buffer::typed("");
        b.edit(LONG, true);
        b.confirm();
        // The user deletes everything, then Undo brings the script back.
        b.edit("", false);
        assert!(!b.asks());
        b.edit(LONG, false);
        assert!(!b.asks(), "undo put back the text the user confirmed");
        // Deleting a line and undoing it, and redoing the deletion.
        let shorter = LONG.replace("print(\"done\");\n", "");
        b.edit(&shorter, false);
        b.edit(LONG, false);
        b.edit(&shorter, false);
        assert!(!b.asks());
    }

    #[test]
    fn undoing_a_paste_gives_the_users_own_text_back_and_redo_asks_again() {
        let mut b = Buffer::typed("print(1);\n");
        let before = b.text.clone();
        let pasted = format!("{before}{HOSTILE}\n");
        b.edit(&pasted, true);
        assert!(b.asks());
        // Undo: the buffer is the user's own text again.
        b.edit(&before, false);
        assert!(!b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Typed);
        // Redo: the pasted text was never confirmed, so it asks.
        b.edit(&pasted, false);
        assert!(b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Clipboard);
    }

    #[test]
    fn undo_cannot_bring_back_foreign_text_the_user_never_confirmed() {
        // A file's text was opened, never confirmed, then the user wrote
        // something else; Undo to the file's text asks.
        let mut b = Buffer::typed("");
        b.edit(LONG, true);
        assert!(b.asks());
        b.edit("", false);
        b.type_str("print(2);");
        assert!(!b.asks(), "rewritten key by key is the user's");
        b.edit(LONG, false);
        assert!(b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Clipboard);
    }

    /// A long typed script with a short one pasted after it, as the window
    /// sees it: the typed text is vouched, the paste is not.
    fn typed_then_pasted() -> Buffer {
        let mut b = Buffer::typed(LONG);
        let pasted = format!("{LONG}{HOSTILE}\n");
        b.edit(&pasted, true);
        assert!(b.asks());
        b
    }

    #[test]
    fn deleting_the_typed_text_around_a_paste_still_asks() {
        // The bypass: the paste was judged against the whole buffer, so
        // deleting the longer typed text counted as rewriting it.
        let mut b = typed_then_pasted();
        b.edit(&format!("{HOSTILE}\n"), false);
        assert!(b.asks(), "exactly the pasted script is left");
        assert_eq!(b.gate.provenance(), Provenance::Clipboard);
        // Backspacing it a character at a time from the front too.
        let mut b = typed_then_pasted();
        while b.text.len() > HOSTILE.len() + 1 {
            let next = b.text[1..].to_string();
            b.edit(&next, false);
            assert!(b.asks(), "{:?}", b.text);
        }
        // The text left is not vouched for: nothing makes it run unasked.
        assert!(!b.known.contains(&b.text));
    }

    #[test]
    fn typing_around_a_paste_keeps_asking_and_rewriting_it_clears() {
        let mut b = typed_then_pasted();
        // Key by key, as the editor reports it: a comment in front of
        // everything, a line after it.
        for (at, c) in "// note\n".chars().enumerate() {
            let mut next = b.text.clone();
            next.insert(at, c);
            b.edit(&next, false);
        }
        b.type_str("print(\"after\");\n");
        assert!(b.asks());
        // Backspace through the pasted line: once fewer than 12 bytes of
        // it are left unbroken it is the user's own rewrite.
        let at = b.text.find(HOSTILE).unwrap();
        for _ in 0..HOSTILE.len() / 2 + 2 {
            let mut next = b.text.clone();
            next.remove(at);
            b.edit(&next, false);
        }
        assert!(!b.asks());
        assert_eq!(b.gate.provenance(), Provenance::Typed);
    }

    #[test]
    fn undo_and_redo_of_a_paste_after_typed_text_stay_as_designed() {
        let mut b = typed_then_pasted();
        let pasted = b.text.clone();
        // Undo: back to the typed text, which the gate vouched for.
        b.edit(LONG, false);
        assert!(!b.asks());
        // Redo: the pasted text again, which asks, and so does deleting the
        // typed part of it.
        b.edit(&pasted, false);
        assert!(b.asks());
        b.edit(&format!("{HOSTILE}\n"), false);
        assert!(b.asks());
    }

    #[test]
    fn the_same_text_changes_nothing() {
        let mut b = Buffer::typed(LONG);
        b.edit(LONG, true);
        assert!(!b.asks(), "pasting what was selected inserts nothing");
    }

    #[test]
    fn the_tap_sees_a_paste_and_only_a_paste() {
        struct Fake(RefCell<Option<String>>);
        impl Clipboard for Fake {
            fn text(&self) -> Option<String> {
                self.0.borrow().clone()
            }
            fn set_text(&self, text: &str) {
                *self.0.borrow_mut() = Some(text.to_string());
            }
        }
        let tap = PasteTap::default();
        let clipboard = TappedClipboard::new(Box::new(Fake(RefCell::new(None))), tap.clone());
        assert!(!tap.take());
        // Copy and Cut write; an empty clipboard reads nothing and pastes nothing.
        clipboard.set_text("copied");
        assert!(!tap.take());
        clipboard.set_text("");
        assert_eq!(clipboard.text().as_deref(), Some(""));
        assert!(!tap.take(), "an empty clipboard pastes nothing");
        clipboard.set_text("x");
        assert_eq!(clipboard.text().as_deref(), Some("x"));
        assert!(tap.take());
        assert!(!tap.take(), "taken once");
    }
}
