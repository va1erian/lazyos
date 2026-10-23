//! xui keys as the page's engine wants them: X11 key symbols, and the modifier bits of
//! `os.lazy.webd.light.v1` (`xui_webd_client::mods`). The same table `xui_webkit::keys` holds for
//! LazyWeb; `xui-app` cannot depend on that crate (it depends on `xui-app`).

use xui_core::{Key, Modifiers};
use xui_webd_client::mods;

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
        _ => return None,
    })
}

/// The key symbol of a letter pressed with Control held (Ctrl+C, Ctrl+A): the lower-case letter
/// itself, which is what the engine's Copy shortcut looks for. `None` for any other key.
pub fn ctrl_letter(key: Key) -> Option<u32> {
    // The virtual-key codes of the letters are their upper-case ASCII.
    let code = u32::from(key.code());
    (0x41..=0x5a).contains(&code).then_some(code + 0x20)
}

/// The key symbol of a character: ASCII and Latin-1 are their own code, other Unicode sits at
/// `0x0100_0000 + code point`.
pub fn char_keysym(c: char) -> u32 {
    let code = u32::from(c);
    match code {
        0x20..=0x7e | 0xa0..=0xff => code,
        _ => 0x0100_0000 + code,
    }
}

/// The modifier bits of a key or mouse event.
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
    fn navigation_keys_have_symbols_and_letters_do_not() {
        assert_eq!(keysym(Key::PAGE_DOWN), Some(0xff56));
        assert_eq!(keysym(Key::from_code(0x43)), None);
    }

    #[test]
    fn a_control_letter_is_its_lower_case_symbol() {
        assert_eq!(ctrl_letter(Key::from_code(0x43)), Some(u32::from(b'c')));
        assert_eq!(ctrl_letter(Key::from_code(0x41)), Some(u32::from(b'a')));
        assert_eq!(ctrl_letter(Key::from_code(0x5a)), Some(u32::from(b'z')));
        assert_eq!(ctrl_letter(Key::RETURN), None);
    }

    #[test]
    fn characters_map_to_their_code_or_the_unicode_range() {
        assert_eq!(char_keysym('a'), 0x61);
        assert_eq!(char_keysym('\u{e9}'), 0xe9);
        assert_eq!(char_keysym('\u{20ac}'), 0x0100_0000 + 0x20ac);
    }

    #[test]
    fn modifier_bits_follow_the_interface() {
        let modifiers = Modifiers {
            shift: true,
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(modifier_bits(modifiers), mods::SHIFT | mods::CTRL);
    }
}
