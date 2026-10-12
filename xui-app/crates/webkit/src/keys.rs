//! xui keys as the page's engine wants them: X11 key symbols.
//!
//! WebKit's libwpe input takes the key symbol a keyboard layout produced (the
//! same numbers xkbcommon uses): ASCII and Latin-1 characters are their own
//! code, other Unicode is `0x0100_0000 + code point`, and the keys that type
//! nothing sit at `0xff00`. Printable keys reach the page through
//! [`char_keysym`] with the text xui translated (the layout is the keyboard
//! service's, not ours); this module names the rest.

use xui_core::{Key, Modifiers};

use crate::engine::mods;

/// The key symbol of a key that types no text, or `None` for every other key.
pub fn keysym(key: Key) -> Option<u32> {
    Some(match key {
        Key::BACK => 0xff08,
        Key::TAB => 0xff09,
        Key::RETURN => 0xff0d,
        Key::ESCAPE => 0xff1b,
        Key::HOME => 0xff50,
        Key::LEFT => 0xff51,
        Key::UP => 0xff52,
        Key::RIGHT => 0xff53,
        Key::DOWN => 0xff54,
        Key::PAGE_UP => 0xff55,
        Key::PAGE_DOWN => 0xff56,
        Key::END => 0xff57,
        Key::INSERT => 0xff63,
        Key::DELETE => 0xffff,
        other => {
            let code = other.code();
            let f1 = Key::F1.code();
            if (f1..=Key::F12.code()).contains(&code) {
                return Some(0xffbe + u32::from(code - f1));
            }
            return None;
        }
    })
}

/// The key symbol of a character.
pub fn char_keysym(c: char) -> u32 {
    let code = u32::from(c);
    match code {
        0x20..=0x7e | 0xa0..=0xff => code,
        _ => 0x0100_0000 + code,
    }
}

/// The key symbol of a letter or digit key held with Ctrl or Alt (a shortcut,
/// which types nothing): letters are lower case, as a layout reports them.
pub fn chord_keysym(key: Key) -> Option<u32> {
    let code = u32::from(key.code());
    match code {
        0x41..=0x5a => Some(code + 0x20),
        0x30..=0x39 => Some(code),
        _ => None,
    }
}

/// xui's modifier state as the engine's bits.
pub fn modifier_bits(modifiers: Modifiers) -> u32 {
    let mut bits = 0;
    if modifiers.shift {
        bits |= mods::SHIFT;
    }
    if modifiers.ctrl {
        bits |= mods::CTRL;
    }
    if modifiers.alt {
        bits |= mods::ALT;
    }
    if modifiers.win {
        bits |= mods::META;
    }
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_type_nothing_have_their_x11_symbols() {
        assert_eq!(keysym(Key::RETURN), Some(0xff0d));
        assert_eq!(keysym(Key::BACK), Some(0xff08));
        assert_eq!(keysym(Key::LEFT), Some(0xff51));
        assert_eq!(keysym(Key::DELETE), Some(0xffff));
        assert_eq!(keysym(Key::F1), Some(0xffbe));
        assert_eq!(keysym(Key::F12), Some(0xffc9));
    }

    #[test]
    fn printable_keys_are_left_to_the_text() {
        for key in [Key::A, Key::Z, Key::DIGIT0, Key::SPACE] {
            assert_eq!(keysym(key), None);
        }
    }

    #[test]
    fn characters_are_their_own_symbol_up_to_latin_1() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym(' '), 0x20);
        assert_eq!(char_keysym('é'), 0xe9);
        assert_eq!(char_keysym('€'), 0x0100_20ac);
    }

    #[test]
    fn chords_use_lower_case_letters_and_plain_digits() {
        assert_eq!(chord_keysym(Key::A), Some(0x61));
        assert_eq!(chord_keysym(Key::Z), Some(0x7a));
        assert_eq!(chord_keysym(Key::DIGIT5), Some(0x35));
        assert_eq!(chord_keysym(Key::RETURN), None);
    }

    #[test]
    fn modifiers_map_to_bits() {
        let all = Modifiers {
            ctrl: true,
            shift: true,
            alt: true,
            win: true,
        };
        assert_eq!(modifier_bits(all), 0b1111);
        assert_eq!(modifier_bits(Modifiers::NONE), 0);
    }
}
