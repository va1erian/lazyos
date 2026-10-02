//! LazyOS key events to doomgeneric key codes (`doomkeys.h`).
//!
//! Two sources reach a client window (`docs/input-plan.md`):
//!
//! - the `inputd` session ([`from_session`]): the physical key as a HID usage
//!   plus its keysym under the active layout, with modifiers as real keys. Ctrl
//!   fires, Shift runs and Alt strafes, as in the original game;
//! - the compositor's legacy `KeyDown`/`KeyUp` ([`from_legacy`]), when `inputd`
//!   is not running. That path never reports a modifier on its own, so `F`
//!   fires and `R` runs there instead (the stopgap the plan describes).
//!
//! Letters come from the keysym, so a layout's letters type what the user sees
//! (menus answer `y`/`n`, cheats are typed), while digits and the strafe keys
//! `,` `.` come from the physical key, so weapon selection works on layouts
//! whose number row is shifted (AZERTY).

/// doomgeneric's key codes (`doomkeys.h`); printable keys are their
/// lowercase ASCII value.
pub mod doom {
    pub const RIGHT: u8 = 0xae;
    pub const LEFT: u8 = 0xac;
    pub const UP: u8 = 0xad;
    pub const DOWN: u8 = 0xaf;
    pub const STRAFE_L: u8 = 0xa0;
    pub const STRAFE_R: u8 = 0xa1;
    pub const USE: u8 = 0xa2;
    pub const FIRE: u8 = 0xa3;
    pub const ESCAPE: u8 = 27;
    pub const ENTER: u8 = 13;
    pub const TAB: u8 = 9;
    pub const BACKSPACE: u8 = 0x7f;
    pub const PAUSE: u8 = 0xff;
    pub const RSHIFT: u8 = 0x80 + 0x36;
    pub const RALT: u8 = 0x80 + 0x38;
    pub const HOME: u8 = 0x80 + 0x47;
    pub const END: u8 = 0x80 + 0x4f;
    pub const PGUP: u8 = 0x80 + 0x49;
    pub const PGDN: u8 = 0x80 + 0x51;
    pub const INS: u8 = 0x80 + 0x52;
    pub const DEL: u8 = 0x80 + 0x53;

    /// `F1`..`F12` (`F11`/`F12` are not contiguous with `F10`).
    pub fn function(n: u32) -> Option<u8> {
        match n {
            1..=10 => Some(0x80 + 0x3a + n as u8),
            11 => Some(0x80 + 0x57),
            12 => Some(0x80 + 0x58),
            _ => None,
        }
    }
}

/// HID keyboard usages (page 7) the session path reads physically.
mod hid {
    pub const DIGIT_1: u32 = 0x1e;
    pub const DIGIT_0: u32 = 0x27;
    pub const COMMA: u32 = 0x36;
    pub const PERIOD: u32 = 0x37;
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

/// A printable ASCII character as Doom wants it: lowercase, Space excluded
/// (it is the use key).
fn printable(value: u32) -> Option<u8> {
    let byte = u8::try_from(value).ok()?;
    (byte.is_ascii_graphic()).then(|| byte.to_ascii_lowercase())
}

/// A key from the `inputd` session: `code` is the HID usage, `sym` its keysym.
pub fn from_session(code: u32, sym: u32) -> Option<u8> {
    match code {
        hid::LEFT_CTRL | hid::RIGHT_CTRL => return Some(doom::FIRE),
        hid::LEFT_SHIFT | hid::RIGHT_SHIFT => return Some(doom::RSHIFT),
        hid::LEFT_ALT | hid::RIGHT_ALT => return Some(doom::RALT),
        hid::COMMA => return Some(doom::STRAFE_L),
        hid::PERIOD => return Some(doom::STRAFE_R),
        hid::DIGIT_1..=hid::DIGIT_0 => {
            let digit = (code - hid::DIGIT_1 + 1) % 10;
            return Some(b'0' + digit as u8);
        }
        _ => {}
    }
    Some(match sym {
        0x20 => doom::USE,
        sym::LEFT => doom::LEFT,
        sym::RIGHT => doom::RIGHT,
        sym::UP => doom::UP,
        sym::DOWN => doom::DOWN,
        sym::ENTER | sym::KP_ENTER => doom::ENTER,
        sym::ESCAPE => doom::ESCAPE,
        sym::TAB => doom::TAB,
        sym::BACKSPACE => doom::BACKSPACE,
        sym::PAUSE => doom::PAUSE,
        sym::HOME => doom::HOME,
        sym::END => doom::END,
        sym::PAGE_UP => doom::PGUP,
        sym::PAGE_DOWN => doom::PGDN,
        sym::INSERT => doom::INS,
        sym::DELETE => doom::DEL,
        sym::F1..=sym::F12 => doom::function(sym - sym::F1 + 1)?,
        other => printable(other)?,
    })
}

/// A key from the compositor's legacy `KeyDown`/`KeyUp`; modifier bits in the
/// top byte are ignored.
pub fn from_legacy(key: u32) -> Option<u8> {
    let code = key & legacy::CODE_MASK;
    Some(match code {
        legacy::SPACE => doom::USE,
        legacy::LEFT => doom::LEFT,
        legacy::RIGHT => doom::RIGHT,
        legacy::UP => doom::UP,
        legacy::DOWN => doom::DOWN,
        legacy::ENTER => doom::ENTER,
        legacy::ESCAPE => doom::ESCAPE,
        legacy::TAB => doom::TAB,
        legacy::BACKSPACE => doom::BACKSPACE,
        legacy::HOME => doom::HOME,
        legacy::END => doom::END,
        legacy::PAGE_UP => doom::PGUP,
        legacy::PAGE_DOWN => doom::PGDN,
        legacy::INSERT => doom::INS,
        legacy::DELETE => doom::DEL,
        // Never forwarded today; mapped in case a compositor starts to.
        legacy::CTRL => doom::FIRE,
        legacy::SHIFT => doom::RSHIFT,
        legacy::ALT => doom::RALT,
        legacy::F1..=legacy::F12 => doom::function(code - legacy::F1 + 1)?,
        _ => match printable(code)? {
            b',' => doom::STRAFE_L,
            b'.' => doom::STRAFE_R,
            b'f' => doom::FIRE,
            b'r' => doom::RSHIFT,
            other => other,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_modifiers_are_the_classic_controls() {
        assert_eq!(from_session(0xe0, 0xffe3), Some(doom::FIRE));
        assert_eq!(from_session(0xe4, 0xffe4), Some(doom::FIRE));
        assert_eq!(from_session(0xe1, 0xffe1), Some(doom::RSHIFT));
        assert_eq!(from_session(0xe6, 0xffea), Some(doom::RALT));
    }

    #[test]
    fn session_movement_and_menu_keys() {
        assert_eq!(from_session(0x52, sym::UP), Some(doom::UP));
        assert_eq!(from_session(0x50, sym::LEFT), Some(doom::LEFT));
        assert_eq!(from_session(0x2c, 0x20), Some(doom::USE));
        assert_eq!(from_session(0x28, sym::ENTER), Some(doom::ENTER));
        assert_eq!(from_session(0x58, sym::KP_ENTER), Some(doom::ENTER));
        assert_eq!(from_session(0x29, sym::ESCAPE), Some(doom::ESCAPE));
        assert_eq!(from_session(0x2b, sym::TAB), Some(doom::TAB));
        assert_eq!(from_session(0x36, b',' as u32), Some(doom::STRAFE_L));
        assert_eq!(from_session(0x37, b'.' as u32), Some(doom::STRAFE_R));
    }

    #[test]
    fn session_letters_follow_the_layout_and_are_lowercase() {
        // AZERTY: the key at QWERTY's `q` types `a`.
        assert_eq!(from_session(0x14, b'a' as u32), Some(b'a'));
        assert_eq!(from_session(0x1c, b'Y' as u32), Some(b'y'));
    }

    #[test]
    fn session_digits_are_physical() {
        // AZERTY's unshifted `1` key types `&`; it must still pick weapon 1.
        assert_eq!(from_session(0x1e, b'&' as u32), Some(b'1'));
        assert_eq!(from_session(0x26, b'_' as u32), Some(b'9'));
        assert_eq!(from_session(0x27, 0xe0), Some(b'0'));
    }

    #[test]
    fn function_keys_skip_to_f11() {
        assert_eq!(from_session(0x3a, sym::F1), Some(0x80 + 0x3b));
        assert_eq!(from_session(0x43, sym::F1 + 9), Some(0x80 + 0x44));
        assert_eq!(from_session(0x44, sym::F1 + 10), Some(0x80 + 0x57));
        assert_eq!(from_session(0x45, sym::F12), Some(0x80 + 0x58));
        assert_eq!(from_legacy(legacy::F1 + 1), Some(0x80 + 0x3c));
        assert_eq!(from_legacy(legacy::F12), Some(0x80 + 0x58));
    }

    #[test]
    fn unknown_session_keys_are_dropped() {
        assert_eq!(from_session(0x39, 0xffe5), None); // Caps Lock
        assert_eq!(from_session(0x01, 0), None);
    }

    #[test]
    fn legacy_keys_use_the_stopgap_bindings() {
        assert_eq!(from_legacy(b'f' as u32), Some(doom::FIRE));
        assert_eq!(from_legacy(b'F' as u32 | (1 << 24)), Some(doom::FIRE));
        assert_eq!(from_legacy(b'r' as u32), Some(doom::RSHIFT));
        assert_eq!(from_legacy(b',' as u32), Some(doom::STRAFE_L));
        assert_eq!(from_legacy(legacy::SPACE), Some(doom::USE));
        assert_eq!(from_legacy(legacy::UP | (1 << 25)), Some(doom::UP));
        assert_eq!(from_legacy(b'Y' as u32), Some(b'y'));
        assert_eq!(from_legacy(0x1ff), None);
    }
}
