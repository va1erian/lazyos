//! LazyOS key events to Quake keynums (`keys.c`'s `K_*`) and the character
//! the layout typed.
//!
//! Two sources reach a client window (`docs/input-plan.md`), exactly as the
//! Doom port maps them; Quake's keynums are printable ASCII plus the
//! `K_*` table (`quake_rs::keys`), and the protocol's `Key` record carries
//! both `keynum` and the character (`ch`) `keys.c` stores for the console
//! and the binds, so `y`/`n` answer menus and cheats are typed on the
//! layout the user is on.
//!
//! Arrows, Escape, Enter and friends come from the keysym; digits and the
//! modifier keys stay physical (`K_CTRL` fires, `K_ALT` strafes: id's 1996
//! binds), so weapon selection works on layouts whose number row is
//! shifted (AZERTY). The Doom rules carry straight over except where the
//! two games' tables differ: Quake has no dedicated use key (Space is a
//! plain key bound to `+jump`), and its backspace is 0x7f, not 8.

/// Quake's keynums (`quake_rs/src/keys.rs`, keys.c's table); printable keys
/// are their ASCII values.
pub mod quake {
    pub const BACKSPACE: u8 = 127;
    pub const UP: u8 = 128;
    pub const DOWN: u8 = 129;
    pub const LEFT: u8 = 130;
    pub const RIGHT: u8 = 131;
    pub const ALT: u8 = 132;
    pub const CTRL: u8 = 133;
    pub const SHIFT: u8 = 134;
    /// `K_F1`, then `K_F2`..`K_F12` follow at +1 (unlike Doom, contiguous).
    pub const F1: u8 = 135;
    pub const F12: u8 = 146;
    pub const INS: u8 = 147;
    pub const DEL: u8 = 148;
    pub const PGDN: u8 = 149;
    pub const PGUP: u8 = 150;
    pub const HOME: u8 = 151;
    pub const END: u8 = 152;
    pub const PAUSE: u8 = 255;
}

/// HID keyboard usages (page 7) the session path reads physically.
mod hid {
    pub const DIGIT_1: u32 = 0x1e;
    pub const DIGIT_0: u32 = 0x27;
    pub const LEFT_CTRL: u32 = 0xe0;
    pub const LEFT_SHIFT: u32 = 0xe1;
    pub const LEFT_ALT: u32 = 0xe2;
    pub const RIGHT_CTRL: u32 = 0xe4;
    pub const RIGHT_SHIFT: u32 = 0xe5;
    pub const RIGHT_ALT: u32 = 0xe6;
}

/// X11 keysyms (`inputmap::keysym`) for the non-printing keys.
mod sym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const ENTER: u32 = 0xff0d;
    pub const PAUSE: u32 = 0xff13;
    pub const ESCAPE: u32 = 0xff1b;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const INSERT: u32 = 0xff63;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const F1: u32 = 0xffbe;
    pub const F12: u32 = 0xffc9;
    pub const DELETE: u32 = 0xffff;
}

/// The compositor's legacy key codes (`kernel/src/display.rs::key`).
mod legacy {
    pub const BACKSPACE: u32 = 8;
    pub const TAB: u32 = 9;
    pub const ENTER: u32 = 13;
    pub const ESCAPE: u32 = 27;
    pub const SPACE: u32 = 32;
    pub const LEFT: u32 = 0x100;
    pub const RIGHT: u32 = 0x101;
    pub const UP: u32 = 0x102;
    pub const DOWN: u32 = 0x103;
    pub const PAGE_UP: u32 = 0x104;
    pub const PAGE_DOWN: u32 = 0x105;
    pub const HOME: u32 = 0x106;
    pub const END: u32 = 0x107;
    pub const SHIFT: u32 = 0x108;
    pub const CTRL: u32 = 0x109;
    pub const ALT: u32 = 0x10a;
    pub const DELETE: u32 = 0x10c;
    pub const INSERT: u32 = 0x10d;
    pub const F1: u32 = 0x110;
    pub const F12: u32 = 0x11b;
    /// Modifier bits the compositor ORs into forwarded keys (bits 24..=27).
    pub const CODE_MASK: u32 = 0x00ff_ffff;
}

/// A key edge as the engine's protocol takes it: `(keynum, ch)`, the
/// character 0 when the key types none.
pub type QuakeKey = (u8, u32);

/// A printable ASCII character as Quake wants it: lowercase, Space excluded
/// (it is a plain key, id's `+jump`, and binds come from `keynum`).
fn printable_char(value: u32) -> Option<u32> {
    let byte = u8::try_from(value).ok()?;
    (byte.is_ascii_graphic()).then(|| u32::from(byte.to_ascii_lowercase()))
}

fn function_key(n: u32) -> u8 {
    quake::F1 + (n - 1) as u8
}

/// A key from the `inputd` session: `code` is the HID usage, `sym` its
/// keysym. Returns the Quake keynum and the character the layout typed.
pub fn from_session(code: u32, sym: u32) -> Option<QuakeKey> {
    match code {
        hid::LEFT_CTRL | hid::RIGHT_CTRL => return Some((quake::CTRL, 0)),
        hid::LEFT_SHIFT | hid::RIGHT_SHIFT => return Some((quake::SHIFT, 0)),
        hid::LEFT_ALT | hid::RIGHT_ALT => return Some((quake::ALT, 0)),
        hid::DIGIT_1..=hid::DIGIT_0 => {
            let digit = (code - hid::DIGIT_1 + 1) % 10;
            return Some((b'0' + digit as u8, u32::from(b'0' + digit as u8)));
        }
        _ => {}
    }
    Some(match sym {
        0x20 => (0x20, 0),
        sym::LEFT => (quake::LEFT, 0),
        sym::RIGHT => (quake::RIGHT, 0),
        sym::UP => (quake::UP, 0),
        sym::DOWN => (quake::DOWN, 0),
        sym::ENTER | sym::KP_ENTER => (13, 0),
        sym::ESCAPE => (27, 0),
        sym::TAB => (9, 0),
        sym::BACKSPACE => (quake::BACKSPACE, 0),
        sym::PAUSE => (quake::PAUSE, 0),
        sym::HOME => (quake::HOME, 0),
        sym::END => (quake::END, 0),
        sym::PAGE_UP => (quake::PGUP, 0),
        sym::PAGE_DOWN => (quake::PGDN, 0),
        sym::INSERT => (quake::INS, 0),
        sym::DELETE => (quake::DEL, 0),
        sym::F1..=sym::F12 => (function_key(sym - sym::F1 + 1), 0),
        other => {
            let ch = printable_char(other)?;
            (ch as u8, ch)
        }
    })
}

/// A key from the compositor's legacy `KeyDown`/`KeyUp`; modifier bits in
/// the top byte are ignored. The legacy path never reports a modifier on
/// its own; plain keys keep their Quake keynum, which is what the game's
/// stored binds read (`~/.apps/.../config.cfg`).
pub fn from_legacy(key: u32) -> Option<QuakeKey> {
    let code = key & legacy::CODE_MASK;
    Some(match code {
        legacy::LEFT => (quake::LEFT, 0),
        legacy::RIGHT => (quake::RIGHT, 0),
        legacy::UP => (quake::UP, 0),
        legacy::DOWN => (quake::DOWN, 0),
        legacy::ENTER => (13, 0),
        legacy::ESCAPE => (27, 0),
        legacy::TAB => (9, 0),
        legacy::SPACE => (0x20, 0),
        legacy::BACKSPACE => (quake::BACKSPACE, 0),
        legacy::HOME => (quake::HOME, 0),
        legacy::END => (quake::END, 0),
        legacy::PAGE_UP => (quake::PGUP, 0),
        legacy::PAGE_DOWN => (quake::PGDN, 0),
        legacy::INSERT => (quake::INS, 0),
        legacy::DELETE => (quake::DEL, 0),
        // Never forwarded today; mapped in case a compositor starts to.
        legacy::CTRL => (quake::CTRL, 0),
        legacy::SHIFT => (quake::SHIFT, 0),
        legacy::ALT => (quake::ALT, 0),
        legacy::F1..=legacy::F12 => (function_key(code - legacy::F1 + 1), 0),
        _ => {
            let ch = printable_char(code)?;
            (ch as u8, ch)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_modifiers_are_the_classic_controls() {
        assert_eq!(from_session(0xe0, 0xffe3), Some((quake::CTRL, 0)));
        assert_eq!(from_session(0xe4, 0xffe4), Some((quake::CTRL, 0)));
        assert_eq!(from_session(0xe1, 0xffe1), Some((quake::SHIFT, 0)));
        assert_eq!(from_session(0xe6, 0xffea), Some((quake::ALT, 0)));
    }

    #[test]
    fn movement_and_menu_keys() {
        assert_eq!(from_session(0x52, sym::UP), Some((quake::UP, 0)));
        assert_eq!(from_session(0x50, sym::LEFT), Some((quake::LEFT, 0)));
        assert_eq!(from_session(0x39, 0x20), Some((0x20, 0)), "Space is jump, its own key");
        assert_eq!(from_session(0x28, sym::ENTER), Some((13, 0)));
        assert_eq!(from_session(0x58, sym::KP_ENTER), Some((13, 0)));
        assert_eq!(from_session(0x29, sym::ESCAPE), Some((27, 0)));
        assert_eq!(from_session(0x2b, sym::TAB), Some((9, 0)));
    }

    #[test]
    fn letters_follow_the_layout_and_are_lowercase() {
        // AZERTY: the key at QWERTY's `q` types `a`.
        assert_eq!(from_session(0x14, b'a' as u32), Some((b'a' as u8, b'a' as u32)));
        assert_eq!(from_session(0x1c, b'Y' as u32), Some((b'y' as u8, b'y' as u32)));
        assert_eq!(from_session(0x1c, b';' as u32), Some((b';' as u8, b';' as u32)));
    }

    #[test]
    fn digits_are_physical() {
        // AZERTY's unshifted `1` key types `&`; it must still pick weapon 1.
        assert_eq!(from_session(0x1e, b'&' as u32), Some((b'1' as u8, b'1' as u32)));
        assert_eq!(from_session(0x26, b'_' as u32), Some((b'9' as u8, b'9' as u32)));
        assert_eq!(from_session(0x27, 0xe0), Some((b'0' as u8, b'0' as u32)));
    }

    #[test]
    fn function_keys_are_contiguous_in_quake() {
        // Quake's K_F1..K_F12 sit at 135..146, contiguous (unlike Doom's,
        // whose F11/F12 skip away).
        assert_eq!(from_session(0x3a, sym::F1), Some((quake::F1, 0)));
        // HID 0x43/0x44 are F9/F10; F11/F12 live at 0x57/0x58.
        assert_eq!(from_session(0x42, sym::F1 + 8), Some((quake::F1 + 8, 0)));
        assert_eq!(from_session(0x58, sym::F12), Some((quake::F12, 0)));
    }

    #[test]
    fn unknown_session_keys_are_dropped() {
        assert_eq!(from_session(0x39, 0xffe5), None); // Caps Lock
        assert_eq!(from_session(0x01, 0), None);
        assert_eq!(from_session(0x05, 0xff4a), None); // non-ASCII
    }
}
