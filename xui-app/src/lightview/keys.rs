//! xui keys as the page's engine wants them: X11 key symbols, and the modifier bits of
//! `os.lazy.webd.light.v1` (`xui_webd_client::mods`). The same table `xui_webkit::keys` holds for
//! LazyWeb+; this app cannot depend on that crate (it depends on `xui-app`).

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
