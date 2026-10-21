//! Typing or arrival? What an edit of the script buffer is, judged from the
//! text before and after it (`docs/messenger-playground-plan.md` D7).
//!
//! The editor reports only the new text, so the app finds the edit by the
//! common prefix and suffix of the two texts: the span the edit *inserted* is
//! what is left between them. Net growth is not the measure: selecting
//! everything and pasting a shorter script shrinks the buffer and still
//! inserts the whole script.
//!
//! What a keystroke can insert into the editor is one character, or an Enter
//! or Tab, which insert only white space (a line break and the indentation).
//! So an inserted span with two or more characters that are not white space
//! is not typing: it arrived (a paste, a drop, a replace). A one-character
//! paste cannot be told from a key by the text alone, so the window also taps
//! the editor's clipboard reads (`PasteTap` in the app) and passes
//! `read_clipboard`.
//!
//! Undo and Redo put back a text the buffer already held. A text the review
//! gate vouched for (typed by the user, shipped with the app, or confirmed)
//! is remembered in [`Known`]; reaching one again is a [`Kind::Restore`], not
//! an arrival, so undoing a deletion in a script the user confirmed does not
//! ask again, and undoing a paste goes back to the user's own text. A text
//! that was produced while the gate was asking (the pasted one) is never
//! remembered, so Redo of an unconfirmed paste asks again.

use std::collections::VecDeque;

/// What an edit did to the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The text did not change.
    Unchanged,
    /// A keystroke's worth: one character, a line break with its
    /// indentation, a deletion.
    Typing,
    /// The buffer went back to a text the gate had already vouched for.
    Restore,
    /// More than a keystroke arrived.
    Arrival,
}

/// The span an edit replaced: what it removed and what it inserted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice<'a> {
    pub removed: &'a str,
    pub inserted: &'a str,
}

/// The one splice that turns `old` into `new`: the shortest span between
/// their common prefix and common suffix (cut on character boundaries).
pub fn splice<'a>(old: &'a str, new: &'a str) -> Splice<'a> {
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
    Splice {
        removed: &old[prefix..a.len() - suffix],
        inserted: &new[prefix..b.len() - suffix],
    }
}

/// How many characters that are not white space `text` holds.
fn visible(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// Whether the edit inserted more than a keystroke can.
pub fn inserted_more_than_a_key(old: &str, new: &str) -> bool {
    visible(splice(old, new).inserted) >= 2
}

/// Judge the edit from `old` to `new`. `read_clipboard` is whether the editor
/// read the clipboard since the previous edit (a paste ran).
pub fn classify(old: &str, new: &str, read_clipboard: bool, known: &Known) -> Kind {
    if old == new {
        return Kind::Unchanged;
    }
    if known.contains(new) {
        return Kind::Restore;
    }
    let inserted = splice(old, new).inserted;
    let visible = visible(inserted);
    if visible >= 2 || (read_clipboard && visible >= 1) {
        Kind::Arrival
    } else {
        Kind::Typing
    }
}

/// Most texts remembered.
const KNOWN_COUNT: usize = 256;
/// Most bytes remembered in all; a larger text is not remembered at all.
const KNOWN_BYTES: usize = 512 * 1024;

/// The recent texts the review gate vouched for, newest last. Exact texts, not
/// hashes: a collision would let a crafted text skip the review.
#[derive(Debug, Default)]
pub struct Known {
    texts: VecDeque<String>,
    bytes: usize,
}

impl Known {
    /// Remember `text`: the gate does not ask about it as it stands.
    pub fn remember(&mut self, text: &str) {
        if text.len() > KNOWN_BYTES || self.texts.back().is_some_and(|t| t == text) {
            return;
        }
        self.bytes += text.len();
        self.texts.push_back(text.to_owned());
        while self.texts.len() > KNOWN_COUNT || self.bytes > KNOWN_BYTES {
            match self.texts.pop_front() {
                Some(old) => self.bytes -= old.len(),
                None => break,
            }
        }
    }

    /// Whether `text` is exactly one that was remembered.
    pub fn contains(&self, text: &str) -> bool {
        self.texts.iter().any(|t| t == text)
    }

    pub fn len(&self) -> usize {
        self.texts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.texts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(old: &str, new: &str) -> Kind {
        classify(old, new, false, &Known::default())
    }

    #[test]
    fn the_splice_is_what_the_edit_inserted_not_how_much_the_buffer_grew() {
        let s = splice("print(1);", "print(1); evil();");
        assert_eq!((s.removed, s.inserted), ("", " evil();"));
        // Select all, paste something shorter: the buffer shrank.
        let s = splice("let a = 1;\nlet b = 2;\nprint(a + b);\n", "x::y();");
        assert_eq!(s.inserted, "x::y();");
        assert!(s.removed.len() > s.inserted.len());
        // Typing over a selection keeps one character.
        let s = splice("abcd", "b");
        assert_eq!((s.removed, s.inserted), ("abcd", "b"));
        let s = splice("abcd", "a");
        assert_eq!(
            (s.removed, s.inserted),
            ("bcd", ""),
            "a kept character is not inserted"
        );
        assert_eq!(splice("abc", "xbc").inserted, "x");
        // Multi-byte characters are never cut.
        let s = splice("a\u{e9}b", "a\u{e8}b");
        assert_eq!((s.removed, s.inserted), ("\u{e9}", "\u{e8}"));
        assert_eq!(splice("same", "same").inserted, "");
    }

    #[test]
    fn ordinary_typing_is_typing() {
        let mut text = String::new();
        for c in "print(\"hi\");".chars() {
            let next = format!("{text}{c}");
            assert_eq!(kind(&text, &next), Kind::Typing, "{next}");
            text = next;
        }
        // Enter with the indentation copied, Tab, and a deep indent.
        assert_eq!(kind("a;", "a;\n"), Kind::Typing);
        assert_eq!(kind("    a;", "    a;\n    "), Kind::Typing);
        assert_eq!(kind("a", "a    "), Kind::Typing);
        assert_eq!(kind("a", "a\n\t\t\t\t\t\t\t\t\t\t\t\t"), Kind::Typing);
        // A character typed over a selection, a deletion, a backspace.
        assert_eq!(kind("let abc = 1;", "let x = 1;"), Kind::Typing);
        assert_eq!(kind("a long line", "a"), Kind::Typing);
        assert_eq!(kind("print", "prin"), Kind::Typing);
        // Typing a character equal to its neighbours.
        assert_eq!(kind("aa", "aaa"), Kind::Typing);
        assert_eq!(kind("a", "a"), Kind::Unchanged);
    }

    #[test]
    fn more_than_a_key_inserts_is_an_arrival_even_when_the_buffer_shrinks() {
        let long = "let a = 1;\nlet b = 2;\nprint(a + b);\nprint(\"done\");\n";
        // Select all (Ctrl+A) and paste a shorter one-line script.
        assert_eq!(kind(long, "sys::init::stop();"), Kind::Arrival);
        // Paste over part of the selection.
        assert_eq!(
            kind("let a = 1; print(a);", "let a = 1; evil();"),
            Kind::Arrival
        );
        // A paste that is no longer than the 12 bytes the old rule allowed.
        assert_eq!(kind("", "a();"), Kind::Arrival);
        assert_eq!(kind("x", "xab"), Kind::Arrival);
        // A paste into the middle, two lines at once.
        assert_eq!(kind("a\nb\n", "a\nx();\ny();\nb\n"), Kind::Arrival);
    }

    #[test]
    fn a_paste_of_white_space_is_not_a_script() {
        assert_eq!(
            classify("a", "a\n\n\n   \n", true, &Known::default()),
            Kind::Typing
        );
    }

    #[test]
    fn a_clipboard_read_makes_even_one_character_an_arrival() {
        let known = Known::default();
        assert_eq!(classify("ab", "abc", false, &known), Kind::Typing);
        assert_eq!(classify("ab", "abc", true, &known), Kind::Arrival);
        // Pasting what the selection already was changes nothing.
        assert_eq!(classify("ab", "ab", true, &known), Kind::Unchanged);
    }

    #[test]
    fn a_text_the_gate_vouched_for_is_a_restore_not_an_arrival() {
        let mut known = Known::default();
        let confirmed = "let a = 1;\nlet b = 2;\nprint(a + b);\n";
        known.remember(confirmed);
        // The user deleted everything; Undo brings the whole script back.
        assert_eq!(classify("", confirmed, false, &known), Kind::Restore);
        // Even a paste of exactly a known text leaves a known text.
        assert_eq!(classify("x", confirmed, true, &known), Kind::Restore);
        // One character different is a new text.
        let other = confirmed.replace("2", "3");
        assert_eq!(classify("", &other, false, &known), Kind::Arrival);
    }

    #[test]
    fn known_texts_are_bounded_and_exact() {
        let mut known = Known::default();
        assert!(known.is_empty());
        for i in 0..KNOWN_COUNT + 10 {
            known.remember(&format!("text {i}"));
        }
        assert_eq!(known.len(), KNOWN_COUNT);
        assert!(!known.contains("text 0"), "the oldest went");
        assert!(known.contains(&format!("text {}", KNOWN_COUNT + 9)));
        assert!(!known.contains("text"), "a prefix is not a match");
        // The same text twice in a row is one entry.
        known.remember("again");
        known.remember("again");
        assert_eq!(known.len(), KNOWN_COUNT);
        // A text beyond the byte budget is not kept; big ones push others out.
        let huge = "x".repeat(KNOWN_BYTES + 1);
        known.remember(&huge);
        assert!(!known.contains(&huge));
        let big = "y".repeat(KNOWN_BYTES / 2 + 1);
        known.remember(&big);
        known.remember(&format!("{big}!"));
        assert!(
            !known.contains(&big),
            "the budget evicted the older big text"
        );
        assert!(known.bytes <= KNOWN_BYTES);
    }
}
