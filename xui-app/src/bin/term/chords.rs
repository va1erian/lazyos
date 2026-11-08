//! Which bytes a key chord sends: xui's key and modifiers on one side,
//! `lazyterm::keys` (xterm's encodings) on the other. Pure, so it is
//! host-tested.
//!
//! Plain text reaches the Terminal as `Char` events; this covers what has no
//! `Char`: navigation and function keys, and Ctrl or Alt held with text (the
//! backend delivers those as a `KeyDown` alone). The keys are the US layout's:
//! xui's `Key` names the key, not the character a layout makes of it.

use lazyterm::input::Mods;
use lazyterm::keys::{encode_named, encode_text, KeyModes, Named};
use xui_core::{Key, Modifiers};

/// The character `key` types on a US keyboard, with Shift as given; `None`
/// for keys that are not text.
pub fn base_char(key: Key, shift: bool) -> Option<char> {
    let pick = |plain: char, shifted: char| Some(if shift { shifted } else { plain });
    match key.code() {
        code @ 0x41..=0x5a => {
            let letter = code as u8 as char;
            Some(if shift { letter } else { letter.to_ascii_lowercase() })
        }
        0x20 => Some(' '),
        0x30 => pick('0', ')'),
        0x31 => pick('1', '!'),
        0x32 => pick('2', '@'),
        0x33 => pick('3', '#'),
        0x34 => pick('4', '$'),
        0x35 => pick('5', '%'),
        0x36 => pick('6', '^'),
        0x37 => pick('7', '&'),
        0x38 => pick('8', '*'),
        0x39 => pick('9', '('),
        0xba => pick(';', ':'),
        0xbb => pick('=', '+'),
        0xbc => pick(',', '<'),
        0xbd => pick('-', '_'),
        0xbe => pick('.', '>'),
        0xbf => pick('/', '?'),
        0xc0 => pick('`', '~'),
        0xdb => pick('[', '{'),
        0xdc => pick('\\', '|'),
        0xdd => pick(']', '}'),
        0xde => pick('\'', '"'),
        _ => None,
    }
}

/// The non-text key `key` is, if it is one this Terminal encodes.
pub fn named_of(key: Key) -> Option<Named> {
    Some(match key {
        Key::UP => Named::Up,
        Key::DOWN => Named::Down,
        Key::LEFT => Named::Left,
        Key::RIGHT => Named::Right,
        Key::HOME => Named::Home,
        Key::END => Named::End,
        Key::INSERT => Named::Insert,
        Key::DELETE => Named::Delete,
        Key::PAGE_UP => Named::PageUp,
        Key::PAGE_DOWN => Named::PageDown,
        Key::BACK => Named::Backspace,
        Key::TAB => Named::Tab,
        Key::RETURN => Named::Enter,
        Key::ESCAPE => Named::Escape,
        other if (Key::F1.code()..=Key::F12.code()).contains(&other.code()) => {
            Named::F((other.code() - Key::F1.code()) as u8 + 1)
        }
        _ => return None,
    })
}

fn mods_of(m: Modifiers) -> Mods {
    Mods { shift: m.shift, alt: m.alt, ctrl: m.ctrl }
}

/// The bytes for a key press that has no `Char` of its own, or `None`.
///
/// Enter, Backspace and Tab also arrive as `Char`s when nothing is held, so
/// they are encoded here only with Alt or Ctrl (and Shift+Tab, which sends a
/// sequence instead of the tab character).
pub fn chord_bytes(key: Key, mods: Modifiers, modes: KeyModes) -> Option<Vec<u8>> {
    let m = mods_of(mods);
    if let Some(named) = named_of(key) {
        let has_char = matches!(named, Named::Enter | Named::Backspace | Named::Tab);
        let held = mods.alt || mods.ctrl || (named == Named::Tab && mods.shift);
        if has_char && !held {
            return None;
        }
        return encode_named(named, m, modes);
    }
    // Ctrl or Alt with text. Without them the text arrives as a `Char`.
    if !(mods.ctrl || mods.alt) {
        return None;
    }
    encode_text(base_char(key, mods.shift)?, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: Modifiers = Modifiers { ctrl: false, shift: false, alt: false, win: false };
    fn mods(ctrl: bool, shift: bool, alt: bool) -> Modifiers {
        Modifiers { ctrl, shift, alt, win: false }
    }
    fn letter(c: u8) -> Key {
        Key::from_code(u16::from(c))
    }
    const NORMAL: KeyModes = KeyModes { app_cursor: false, app_keypad: false };
    const APP: KeyModes = KeyModes { app_cursor: true, app_keypad: false };

    fn bytes(key: Key, m: Modifiers, modes: KeyModes) -> Option<String> {
        chord_bytes(key, m, modes).map(|b| String::from_utf8(b).unwrap().replace('\x1b', "^["))
    }

    #[test]
    fn plain_text_is_left_to_the_char_events() {
        assert_eq!(chord_bytes(letter(b'A'), N, NORMAL), None);
        assert_eq!(chord_bytes(letter(b'A'), mods(false, true, false), NORMAL), None);
        assert_eq!(chord_bytes(Key::RETURN, N, NORMAL), None, "Enter is a Char");
        assert_eq!(chord_bytes(Key::BACK, N, NORMAL), None, "Backspace is a Char");
        assert_eq!(chord_bytes(Key::TAB, N, NORMAL), None, "Tab is a Char");
        assert_eq!(chord_bytes(Key::SPACE, N, NORMAL), None);
    }

    #[test]
    fn alt_is_an_escape_prefix() {
        let alt = mods(false, false, true);
        assert_eq!(bytes(letter(b'F'), alt, NORMAL).as_deref(), Some("^[f"));
        assert_eq!(bytes(letter(b'F'), mods(false, true, true), NORMAL).as_deref(), Some("^[F"));
        assert_eq!(bytes(Key::DIGIT1, alt, NORMAL).as_deref(), Some("^[1"));
        assert_eq!(bytes(Key::from_code(0xbd), alt, NORMAL).as_deref(), Some("^[-"));
        assert_eq!(bytes(Key::BACK, alt, NORMAL).as_deref(), Some("^[\u{7f}"), "Alt+Backspace");
        assert_eq!(bytes(Key::RETURN, alt, NORMAL).as_deref(), Some("^[\r"));
        assert_eq!(bytes(Key::SPACE, alt, NORMAL).as_deref(), Some("^[ "));
        assert_eq!(bytes(Key::DELETE, alt, NORMAL).as_deref(), Some("^[[3;3~"));
    }

    #[test]
    fn ctrl_makes_control_characters() {
        let ctrl = mods(true, false, false);
        assert_eq!(chord_bytes(letter(b'C'), ctrl, NORMAL), Some(vec![3]));
        assert_eq!(chord_bytes(letter(b'Z'), ctrl, NORMAL), Some(vec![26]));
        assert_eq!(chord_bytes(Key::SPACE, ctrl, NORMAL), Some(vec![0]), "Ctrl+Space");
        // Ctrl+Shift+2 is Ctrl+@.
        assert_eq!(chord_bytes(Key::DIGIT2, mods(true, true, false), NORMAL), Some(vec![0]));
        assert_eq!(chord_bytes(Key::from_code(0xdb), ctrl, NORMAL), Some(vec![27]), "Ctrl+[");
        assert_eq!(chord_bytes(Key::from_code(0xdc), ctrl, NORMAL), Some(vec![28]), "Ctrl+\\");
        assert_eq!(chord_bytes(Key::from_code(0xdd), ctrl, NORMAL), Some(vec![29]), "Ctrl+]");
        assert_eq!(chord_bytes(Key::DIGIT6, mods(true, true, false), NORMAL), Some(vec![30]), "Ctrl+^");
        assert_eq!(chord_bytes(Key::from_code(0xbd), mods(true, true, false), NORMAL), Some(vec![31]), "Ctrl+_");
        assert_eq!(chord_bytes(Key::from_code(0xbf), ctrl, NORMAL), Some(vec![31]), "Ctrl+/");
        assert_eq!(chord_bytes(Key::from_code(0xbf), mods(true, true, false), NORMAL), Some(vec![127]), "Ctrl+?");
        assert_eq!(chord_bytes(Key::DIGIT5, ctrl, NORMAL), None, "no control character");
        assert_eq!(chord_bytes(Key::from_code(0xbd), ctrl, NORMAL), None, "Ctrl+- is the zoom");
        assert_eq!(bytes(letter(b'D'), mods(true, false, true), NORMAL).as_deref(), Some("^[\u{4}"), "Ctrl+Alt+d");
        assert_eq!(chord_bytes(Key::BACK, ctrl, NORMAL), Some(vec![8]), "Ctrl+Backspace");
    }

    #[test]
    fn navigation_follows_the_modes() {
        assert_eq!(bytes(Key::UP, N, NORMAL).as_deref(), Some("^[[A"));
        assert_eq!(bytes(Key::UP, N, APP).as_deref(), Some("^[OA"));
        assert_eq!(bytes(Key::RIGHT, mods(true, false, false), NORMAL).as_deref(), Some("^[[1;5C"));
        assert_eq!(bytes(Key::HOME, mods(false, true, false), NORMAL).as_deref(), Some("^[[1;2H"));
        assert_eq!(bytes(Key::END, N, APP).as_deref(), Some("^[OF"));
        assert_eq!(bytes(Key::INSERT, N, NORMAL).as_deref(), Some("^[[2~"));
        assert_eq!(bytes(Key::DELETE, N, NORMAL).as_deref(), Some("^[[3~"));
        assert_eq!(bytes(Key::PAGE_UP, N, NORMAL).as_deref(), Some("^[[5~"));
        assert_eq!(bytes(Key::PAGE_DOWN, mods(true, false, false), NORMAL).as_deref(), Some("^[[6;5~"));
        assert_eq!(bytes(Key::ESCAPE, N, NORMAL).as_deref(), Some("^["));
        assert_eq!(bytes(Key::TAB, mods(false, true, false), NORMAL).as_deref(), Some("^[[Z"), "Shift+Tab");
    }

    #[test]
    fn function_keys() {
        let want = ["^[OP", "^[OQ", "^[OR", "^[OS", "^[[15~", "^[[17~", "^[[18~", "^[[19~", "^[[20~", "^[[21~", "^[[23~", "^[[24~"];
        for (n, want) in want.iter().enumerate() {
            let key = Key::from_code(Key::F1.code() + n as u16);
            assert_eq!(bytes(key, N, NORMAL).as_deref(), Some(*want), "F{}", n + 1);
        }
        assert_eq!(bytes(Key::F5, mods(false, false, true), NORMAL).as_deref(), Some("^[[15;3~"));
        assert_eq!(bytes(Key::F1, mods(true, false, false), NORMAL).as_deref(), Some("^[[1;5P"));
        assert_eq!(chord_bytes(Key::from_code(Key::F12.code() + 1), N, NORMAL), None, "F13 does not exist");
    }

    #[test]
    fn the_us_layout_table() {
        assert_eq!(base_char(Key::DIGIT7, false), Some('7'));
        assert_eq!(base_char(Key::DIGIT7, true), Some('&'));
        assert_eq!(base_char(Key::from_code(0xc0), true), Some('~'));
        assert_eq!(base_char(Key::from_code(0xde), false), Some('\''));
        assert_eq!(base_char(Key::UP, false), None);
        assert_eq!(base_char(Key::F1, false), None);
        assert_eq!(base_char(Key::from_code(0), false), None);
    }
}
