//! Keys to bytes, and the Terminal's own shortcuts. Plain `Ctrl+C` stays
//! `^C`: the copy and paste chords all involve Shift or Insert.

use xui_core::app::Ui;
use xui_core::{Key, Modifiers};

use super::scroll::key_move;
use super::{Msg, Terminal};

/// A key chord the Terminal handles itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shortcut {
    Copy,
    Paste,
    SelectAll,
}

/// The shortcut a chord is, if any: `Ctrl+Shift+C` / `Ctrl+Insert` copy,
/// `Ctrl+Shift+V` / `Shift+Insert` paste, `Ctrl+Shift+A` selects all.
pub fn shortcut(key: Key, m: Modifiers) -> Option<Shortcut> {
    let letter = |c: u8| key.code() == u16::from(c);
    match (m.ctrl, m.shift, m.alt) {
        (true, true, false) if letter(b'C') => Some(Shortcut::Copy),
        (true, true, false) if letter(b'V') => Some(Shortcut::Paste),
        (true, true, false) if letter(b'A') => Some(Shortcut::SelectAll),
        (true, false, false) if key == Key::INSERT => Some(Shortcut::Copy),
        (false, true, false) if key == Key::INSERT => Some(Shortcut::Paste),
        _ => None,
    }
}

/// A zoom chord: Ctrl+= and Ctrl++ (and the keypad's plus) zoom in, Ctrl+-
/// out, Ctrl+0 resets (`Some(None)`). Alt chords are the program's.
pub fn zoom_key(key: Key, m: Modifiers) -> Option<Option<i32>> {
    if !m.ctrl || m.alt {
        return None;
    }
    match key.code() {
        0xbb | 0x6b => Some(Some(1)),
        0xbd | 0x6d => Some(Some(-1)),
        0x30 | 0x60 => Some(None),
        _ => None,
    }
}

/// The control byte of a Ctrl+letter chord (`Ctrl+C` is 3), or `None`.
pub fn control_byte(key: Key) -> Option<u8> {
    let code = key.code();
    (u16::from(b'A')..=u16::from(b'Z'))
        .contains(&code)
        .then(|| code as u8 - b'A' + 1)
}

/// The bytes a VT100 sends for a non-text key (`None`: nothing to send).
pub fn key_sequence(key: Key) -> Option<&'static [u8]> {
    Some(match key {
        Key::ESCAPE => b"\x1b",
        Key::UP => b"\x1b[A",
        Key::DOWN => b"\x1b[B",
        Key::RIGHT => b"\x1b[C",
        Key::LEFT => b"\x1b[D",
        Key::HOME => b"\x1b[H",
        Key::END => b"\x1b[F",
        Key::INSERT => b"\x1b[2~",
        Key::DELETE => b"\x1b[3~",
        Key::PAGE_UP => b"\x1b[5~",
        Key::PAGE_DOWN => b"\x1b[6~",
        _ => return None,
    })
}

impl Terminal {
    /// Enter was sent (typed or pasted): report the line typed so far as the
    /// command, `TERM:CMD`, and start waiting for its output.
    pub(super) fn submitted(&mut self) {
        let line = self.typed.trim().to_string();
        self.typed.clear();
        if !line.is_empty() {
            self.perf.submitted(&self.paint_clock);
            println!("TERM:CMD:{line}");
            self.evidence.submitted(line);
        }
    }

    /// Forward one character to the child, keeping a local echo of the line so
    /// `TERM:CMD` can report the submitted command.
    pub(super) fn key(&mut self, ch: char, ui: &mut Ui<Msg>) {
        match ch {
            '\n' | '\r' => {
                self.input(ui, b"\r");
                self.submitted();
            }
            '\u{8}' => {
                self.typed.pop();
                self.input(ui, b"\x7f");
            }
            // Control characters (^C, ^D, ^Z, ^U, ...) go to the line
            // discipline, which turns them into signals or edits.
            '\t' => self.input(ui, b"\t"),
            // Escape arrives through the key path (`key_sequence`).
            '\u{1b}' => {}
            c if c.is_control() => self.input(ui, &[c as u8]),
            c => {
                self.typed.push(c);
                self.perf.sent_key();
                self.input(ui, c.to_string().as_bytes());
            }
        }
    }

    /// A non-text key: the Terminal's own chords, the scroll keys, Ctrl
    /// letters as control bytes, and the sequences of the navigation keys.
    pub(super) fn key_chord(&mut self, ui: &mut Ui<Msg>, key: Key, mods: Modifiers) {
        if let Some(zoom) = zoom_key(key, mods) {
            return self.zoom_by(ui, zoom);
        }
        match shortcut(key, mods) {
            Some(Shortcut::Copy) => return self.copy(ui),
            Some(Shortcut::Paste) => return self.paste(ui),
            Some(Shortcut::SelectAll) => return self.select_all(ui),
            None => {}
        }
        let scrollable = !self.term.borrow().on_alternate_screen();
        if let Some(mv) = key_move(key, mods.shift, mods.ctrl, self.page()).filter(|_| scrollable) {
            self.scroll(ui, mv);
        } else if mods.ctrl {
            // Ctrl+A..Z: the control byte (^C, ^D, ^Z ...), which the line
            // discipline turns into a signal or an edit.
            if let Some(byte) = control_byte(key) {
                // ^C, ^D and ^U abandon the line being typed.
                if matches!(byte, 3 | 4 | 21) {
                    self.typed.clear();
                }
                self.input(ui, &[byte]);
            }
        } else if let Some(sequence) = key_sequence(key) {
            self.input(ui, sequence);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool) -> Modifiers {
        Modifiers {
            ctrl,
            shift,
            ..Modifiers::default()
        }
    }

    fn letter(c: u8) -> Key {
        Key::from_code(u16::from(c))
    }

    #[test]
    fn the_copy_and_paste_chords() {
        assert_eq!(shortcut(letter(b'C'), mods(true, true)), Some(Shortcut::Copy));
        assert_eq!(shortcut(Key::INSERT, mods(true, false)), Some(Shortcut::Copy));
        assert_eq!(shortcut(letter(b'V'), mods(true, true)), Some(Shortcut::Paste));
        assert_eq!(shortcut(Key::INSERT, mods(false, true)), Some(Shortcut::Paste));
        assert_eq!(shortcut(letter(b'A'), mods(true, true)), Some(Shortcut::SelectAll));
    }

    #[test]
    fn plain_ctrl_letters_stay_control_bytes() {
        assert_eq!(shortcut(letter(b'C'), mods(true, false)), None, "^C interrupts");
        assert_eq!(shortcut(letter(b'V'), mods(true, false)), None);
        assert_eq!(shortcut(letter(b'A'), mods(true, false)), None);
        assert_eq!(shortcut(Key::INSERT, mods(false, false)), None);
        assert_eq!(control_byte(letter(b'C')), Some(3));
        assert_eq!(control_byte(letter(b'Z')), Some(26));
        assert_eq!(control_byte(Key::UP), None);
    }

    #[test]
    fn zoom_chords() {
        let ctrl = mods(true, false);
        assert_eq!(zoom_key(Key::from_code(0xbb), ctrl), Some(Some(1)), "Ctrl+=");
        assert_eq!(zoom_key(Key::from_code(0xbb), mods(true, true)), Some(Some(1)), "Ctrl++");
        assert_eq!(zoom_key(Key::from_code(0x6b), ctrl), Some(Some(1)), "keypad +");
        assert_eq!(zoom_key(Key::from_code(0xbd), ctrl), Some(Some(-1)), "Ctrl+-");
        assert_eq!(zoom_key(Key::from_code(0x30), ctrl), Some(None), "Ctrl+0 resets");
        assert_eq!(zoom_key(Key::from_code(0xbb), mods(false, false)), None, "a plain = is text");
        let alt = Modifiers { ctrl: true, alt: true, ..Modifiers::default() };
        assert_eq!(zoom_key(Key::from_code(0xbb), alt), None);
    }

    #[test]
    fn alt_chords_are_not_shortcuts() {
        let m = Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
            ..Modifiers::default()
        };
        assert_eq!(shortcut(letter(b'C'), m), None);
    }

    #[test]
    fn navigation_keys_have_sequences() {
        assert_eq!(key_sequence(Key::UP), Some(&b"\x1b[A"[..]));
        assert_eq!(key_sequence(Key::INSERT), Some(&b"\x1b[2~"[..]));
        assert_eq!(key_sequence(Key::F1), None);
    }
}
