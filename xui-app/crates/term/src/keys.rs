//! What the terminal sends a program for a key: xterm's encodings (the ones
//! the shipped `xterm-256color` terminfo entry describes), for the modes the
//! program has set (application cursor keys, application keypad).
//!
//! Plain text (letters, digits, symbols) is sent as typed by the host; this
//! module owns the keys that are not text, and what Ctrl and Alt do to text.
//! The shortcuts the Terminal keeps for itself (Ctrl+Shift+C/V/A, the zoom
//! keys, Shift+PageUp for scrollback) are decided before any of this.

use crate::input::Mods;

/// A key that is not text.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Named {
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    /// `F(1)` to `F(12)`.
    F(u8),
    Backspace,
    Tab,
    Enter,
    Escape,
    /// A key of the numeric keypad.
    Keypad(Keypad),
}

/// The keys of the numeric keypad that DECKPAM changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Keypad {
    /// `0` to `9`.
    Digit(u8),
    Enter,
    Plus,
    Minus,
    Multiply,
    Divide,
    Decimal,
}

/// The modes a program sets that change what keys send.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KeyModes {
    /// DECCKM (`CSI ? 1 h`): unmodified arrows, Home and End send `SS3`.
    pub app_cursor: bool,
    /// DECKPAM (`ESC =`): the keypad sends `SS3` sequences.
    pub app_keypad: bool,
}

/// xterm's modifier parameter: 1 plus shift (1), alt (2), ctrl (4).
pub fn modifier_param(mods: Mods) -> u8 {
    1 + u8::from(mods.shift) + 2 * u8::from(mods.alt) + 4 * u8::from(mods.ctrl)
}

fn any(mods: Mods) -> bool {
    mods.shift || mods.alt || mods.ctrl
}

/// `ESC [ 1 ; m F` for a modified key, `ESC [ F` or `ESC O F` unmodified.
fn final_key(letter: u8, mods: Mods, app: bool) -> Vec<u8> {
    if any(mods) {
        format!("\x1b[1;{}{}", modifier_param(mods), letter as char).into_bytes()
    } else if app {
        vec![0x1b, b'O', letter]
    } else {
        vec![0x1b, b'[', letter]
    }
}

/// `ESC [ n ~`, with `;m` when modified.
fn tilde_key(code: u8, mods: Mods) -> Vec<u8> {
    if any(mods) {
        format!("\x1b[{code};{}~", modifier_param(mods)).into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// The bytes for `key` with `mods` held, or `None` when it sends nothing.
pub fn encode_named(key: Named, mods: Mods, modes: KeyModes) -> Option<Vec<u8>> {
    let app = modes.app_cursor;
    Some(match key {
        Named::Up => final_key(b'A', mods, app),
        Named::Down => final_key(b'B', mods, app),
        Named::Right => final_key(b'C', mods, app),
        Named::Left => final_key(b'D', mods, app),
        Named::Home => final_key(b'H', mods, app),
        Named::End => final_key(b'F', mods, app),
        Named::Insert => tilde_key(2, mods),
        Named::Delete => tilde_key(3, mods),
        Named::PageUp => tilde_key(5, mods),
        Named::PageDown => tilde_key(6, mods),
        Named::F(n @ 1..=4) => {
            let letter = b'P' + (n - 1);
            if any(mods) {
                format!("\x1b[1;{}{}", modifier_param(mods), letter as char).into_bytes()
            } else {
                vec![0x1b, b'O', letter]
            }
        }
        Named::F(n @ 5..=12) => {
            // xterm skips 16 and 22.
            const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde_key(CODES[usize::from(n - 5)], mods)
        }
        Named::F(_) => return None,
        Named::Backspace => {
            // DEL; Ctrl+Backspace is BS; Alt prefixes an ESC.
            let byte = if mods.ctrl { 0x08 } else { 0x7f };
            let mut out = if mods.alt { vec![0x1b] } else { Vec::new() };
            out.push(byte);
            out
        }
        Named::Tab if mods.shift && !mods.alt => b"\x1b[Z".to_vec(),
        Named::Tab if mods.shift => b"\x1b\x1b[Z".to_vec(),
        Named::Tab => prefixed(mods, b"\t"),
        Named::Enter => prefixed(mods, b"\r"),
        Named::Escape => prefixed(mods, b"\x1b"),
        Named::Keypad(key) => return Some(keypad(key, modes.app_keypad)),
    })
}

/// `bytes`, with an ESC in front when Alt is held.
fn prefixed(mods: Mods, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 1);
    if mods.alt {
        out.push(0x1b);
    }
    out.extend_from_slice(bytes);
    out
}

/// A keypad key: its character normally, an `SS3` sequence in application
/// keypad mode.
fn keypad(key: Keypad, application: bool) -> Vec<u8> {
    let (plain, ss3) = match key {
        Keypad::Digit(d) => (b'0' + d.min(9), b'p' + d.min(9)),
        Keypad::Enter => (b'\r', b'M'),
        Keypad::Plus => (b'+', b'k'),
        Keypad::Minus => (b'-', b'm'),
        Keypad::Multiply => (b'*', b'j'),
        Keypad::Divide => (b'/', b'o'),
        Keypad::Decimal => (b'.', b'n'),
    };
    if application {
        vec![0x1b, b'O', ss3]
    } else {
        vec![plain]
    }
}

/// The control character for Ctrl plus `c`: letters are 1 to 26, and
/// `@`/Space 0, `[` 27, `\` 28, `]` 29, `^` 30, `_` and `/` 31, `?` 127.
pub fn control_char(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '@' | ' ' => Some(0),
        '[' => Some(27),
        '\\' => Some(28),
        ']' => Some(29),
        '^' => Some(30),
        '_' | '/' => Some(31),
        '?' => Some(127),
        _ => None,
    }
}

/// The bytes for text `c` typed with `mods`: Ctrl makes the control
/// character (when there is one), and Alt puts an ESC in front of whatever
/// results. `None` when Ctrl is held and `c` has no control character.
pub fn encode_text(c: char, mods: Mods) -> Option<Vec<u8>> {
    let mut out = if mods.alt { vec![0x1b] } else { Vec::new() };
    if mods.ctrl {
        out.push(control_char(c)?);
    } else {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

/// The DECSET codes of the modes this module reads, for the tests and the
/// terminfo `smkx`/`rmkx` strings to agree.
pub const DECCKM: u16 = 1;
/// Focus reporting: CSI I / CSI O.
pub const FOCUS_REPORTING: u16 = 1004;
/// Synchronized output.
pub const SYNC_OUTPUT: u16 = 2026;

#[cfg(test)]
mod tests {
    use super::*;

    fn m(shift: bool, alt: bool, ctrl: bool) -> Mods {
        Mods { shift, alt, ctrl }
    }
    const NONE: Mods = Mods { shift: false, alt: false, ctrl: false };
    const NORMAL: KeyModes = KeyModes { app_cursor: false, app_keypad: false };
    const APP: KeyModes = KeyModes { app_cursor: true, app_keypad: true };

    fn enc(key: Named, mods: Mods, modes: KeyModes) -> String {
        String::from_utf8(encode_named(key, mods, modes).unwrap()).unwrap().replace('\x1b', "^[")
    }

    #[test]
    fn the_modifier_parameter_is_xterms() {
        assert_eq!(modifier_param(NONE), 1);
        assert_eq!(modifier_param(m(true, false, false)), 2);
        assert_eq!(modifier_param(m(false, true, false)), 3);
        assert_eq!(modifier_param(m(true, true, false)), 4);
        assert_eq!(modifier_param(m(false, false, true)), 5);
        assert_eq!(modifier_param(m(true, false, true)), 6);
        assert_eq!(modifier_param(m(false, true, true)), 7);
        assert_eq!(modifier_param(m(true, true, true)), 8);
    }

    #[test]
    fn arrows_follow_decckm_until_modified() {
        assert_eq!(enc(Named::Up, NONE, NORMAL), "^[[A");
        assert_eq!(enc(Named::Left, NONE, NORMAL), "^[[D");
        assert_eq!(enc(Named::Up, NONE, APP), "^[OA");
        assert_eq!(enc(Named::Down, NONE, APP), "^[OB");
        assert_eq!(enc(Named::Right, NONE, APP), "^[OC");
        assert_eq!(enc(Named::Left, NONE, APP), "^[OD");
        // A modified arrow is the CSI form whatever the mode.
        assert_eq!(enc(Named::Right, m(false, false, true), NORMAL), "^[[1;5C");
        assert_eq!(enc(Named::Right, m(false, false, true), APP), "^[[1;5C");
        assert_eq!(enc(Named::Up, m(true, false, false), NORMAL), "^[[1;2A");
        assert_eq!(enc(Named::Left, m(false, true, false), NORMAL), "^[[1;3D");
        assert_eq!(enc(Named::Down, m(true, true, true), NORMAL), "^[[1;8B");
    }

    #[test]
    fn home_end_and_the_tilde_keys() {
        assert_eq!(enc(Named::Home, NONE, NORMAL), "^[[H");
        assert_eq!(enc(Named::End, NONE, NORMAL), "^[[F");
        assert_eq!(enc(Named::Home, NONE, APP), "^[OH");
        assert_eq!(enc(Named::End, NONE, APP), "^[OF");
        assert_eq!(enc(Named::Home, m(false, false, true), NORMAL), "^[[1;5H");
        assert_eq!(enc(Named::Insert, NONE, NORMAL), "^[[2~");
        assert_eq!(enc(Named::Delete, NONE, NORMAL), "^[[3~");
        assert_eq!(enc(Named::PageUp, NONE, NORMAL), "^[[5~");
        assert_eq!(enc(Named::PageDown, NONE, APP), "^[[6~", "DECCKM does not change them");
        assert_eq!(enc(Named::Delete, m(true, false, false), NORMAL), "^[[3;2~");
        assert_eq!(enc(Named::PageUp, m(false, false, true), NORMAL), "^[[5;5~");
        assert_eq!(enc(Named::Insert, m(false, true, false), NORMAL), "^[[2;3~");
    }

    #[test]
    fn function_keys() {
        let want = [
            "^[OP", "^[OQ", "^[OR", "^[OS", "^[[15~", "^[[17~", "^[[18~", "^[[19~", "^[[20~",
            "^[[21~", "^[[23~", "^[[24~",
        ];
        for (n, want) in (1..=12).zip(want) {
            assert_eq!(enc(Named::F(n), NONE, NORMAL), want, "F{n}");
            assert_eq!(enc(Named::F(n), NONE, APP), want, "F{n} ignores DECCKM");
        }
        assert_eq!(enc(Named::F(1), m(true, false, false), NORMAL), "^[[1;2P");
        assert_eq!(enc(Named::F(4), m(false, false, true), NORMAL), "^[[1;5S");
        assert_eq!(enc(Named::F(5), m(false, true, false), NORMAL), "^[[15;3~");
        assert_eq!(enc(Named::F(12), m(true, false, true), NORMAL), "^[[24;6~");
        assert_eq!(encode_named(Named::F(0), NONE, NORMAL), None);
        assert_eq!(encode_named(Named::F(13), NONE, NORMAL), None);
    }

    #[test]
    fn backspace_tab_enter_escape() {
        assert_eq!(encode_named(Named::Backspace, NONE, NORMAL), Some(vec![0x7f]));
        assert_eq!(encode_named(Named::Backspace, m(false, true, false), NORMAL), Some(vec![0x1b, 0x7f]));
        assert_eq!(encode_named(Named::Backspace, m(false, false, true), NORMAL), Some(vec![0x08]));
        assert_eq!(enc(Named::Tab, NONE, NORMAL), "\t");
        assert_eq!(enc(Named::Tab, m(true, false, false), NORMAL), "^[[Z", "Shift+Tab is CSI Z");
        assert_eq!(enc(Named::Tab, m(true, true, false), NORMAL), "^[^[[Z");
        assert_eq!(enc(Named::Enter, m(false, true, false), NORMAL), "^[\r");
        assert_eq!(enc(Named::Escape, NONE, NORMAL), "^[");
        assert_eq!(enc(Named::Escape, m(false, true, false), NORMAL), "^[^[");
    }

    #[test]
    fn the_keypad_follows_deckpam() {
        assert_eq!(enc(Named::Keypad(Keypad::Digit(5)), NONE, NORMAL), "5");
        assert_eq!(enc(Named::Keypad(Keypad::Digit(5)), NONE, APP), "^[Ou");
        assert_eq!(enc(Named::Keypad(Keypad::Digit(0)), NONE, APP), "^[Op");
        assert_eq!(enc(Named::Keypad(Keypad::Digit(9)), NONE, APP), "^[Oy");
        assert_eq!(enc(Named::Keypad(Keypad::Enter), NONE, NORMAL), "\r");
        assert_eq!(enc(Named::Keypad(Keypad::Enter), NONE, APP), "^[OM");
        assert_eq!(enc(Named::Keypad(Keypad::Plus), NONE, APP), "^[Ok");
        assert_eq!(enc(Named::Keypad(Keypad::Minus), NONE, APP), "^[Om");
        assert_eq!(enc(Named::Keypad(Keypad::Multiply), NONE, APP), "^[Oj");
        assert_eq!(enc(Named::Keypad(Keypad::Divide), NONE, APP), "^[Oo");
        assert_eq!(enc(Named::Keypad(Keypad::Decimal), NONE, APP), "^[On");
        // A digit beyond 9 cannot overflow into another key.
        assert_eq!(enc(Named::Keypad(Keypad::Digit(200)), NONE, APP), "^[Oy");
    }

    #[test]
    fn ctrl_and_alt_on_text() {
        let ctrl = m(false, false, true);
        let alt = m(false, true, false);
        assert_eq!(encode_text('a', alt), Some(vec![0x1b, b'a']), "Alt+a is ESC a");
        assert_eq!(encode_text('X', alt), Some(vec![0x1b, b'X']));
        assert_eq!(encode_text('\u{e9}', alt), Some(vec![0x1b, 0xc3, 0xa9]));
        assert_eq!(encode_text('c', ctrl), Some(vec![3]));
        assert_eq!(encode_text('Z', ctrl), Some(vec![26]));
        assert_eq!(encode_text('d', m(false, true, true)), Some(vec![0x1b, 4]), "Ctrl+Alt+d");
        assert_eq!(encode_text('5', NONE), Some(b"5".to_vec()));
        assert_eq!(encode_text('5', ctrl), None, "no control character for it");
    }

    #[test]
    fn the_control_characters_beyond_letters() {
        let table = [
            (' ', 0u8),
            ('@', 0),
            ('[', 27),
            ('\\', 28),
            (']', 29),
            ('^', 30),
            ('_', 31),
            ('/', 31),
            ('?', 127),
        ];
        for (c, byte) in table {
            assert_eq!(control_char(c), Some(byte), "Ctrl+{c:?}");
        }
        assert_eq!(control_char('1'), None);
        assert_eq!(control_char('\u{e9}'), None);
        assert_eq!(control_char('a'), Some(1));
        assert_eq!(control_char('z'), Some(26));
    }
}
