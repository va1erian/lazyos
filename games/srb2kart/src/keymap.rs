//! LazyOS key events to SRB2Kart key codes (`keys.h`).
//!
//! Two sources reach a client window (`docs/input-plan.md`):
//!
//! - the `inputd` session ([`from_session`]): the physical key as a HID usage
//!   plus its keysym under the active layout, with modifiers as real keys;
//! - the compositor's legacy `KeyDown`/`KeyUp` ([`from_legacy`]), when `inputd`
//!   is not running. That path never reports a modifier on its own, so Shift
//!   (look back) is unreachable there.
//!
//! Letters come from the keysym, so a layout's letters type what the user sees
//! (console, chat, menus), while digits come from the physical key, so the
//! number row gives digits on layouts whose unshifted row is symbols (AZERTY).
//! Like the SDL interface, printable keys are their lowercase ASCII value.
//!
//! # Default controls (Kart-Public v1.6 `G_Controldefault`, `src/g_input.c`)
//!
//! | Action        | Key                  |
//! |---------------|----------------------|
//! | Steer         | Left / Right arrows  |
//! | Aim up / down | Up / Down arrows     |
//! | Accelerate    | `A`                  |
//! | Drift         | `S`                  |
//! | Brake         | `D`                  |
//! | Use item      | Space                |
//! | Look back     | Left Shift           |
//! | Scoreboard    | Tab                  |
//! | Pause         | Pause/Break          |
//! | Console       | `` ` ``              |
//! | Talk          | `T`                  |
//! | Spectate      | `'`                  |
//! | Look up/down  | Page Up / Page Down  |
//! | Center view   | End                  |
//! | Reset camera  | Home                 |
//! | Toggle camera | Backspace            |
//! | Screenshot / GIF / viewpoint | F8 / F9 / F12 |

/// SRB2Kart's key codes: `src/keys.h` of Kart-Public v1.6 (ASCII for
/// printable keys, `0x80 + scancode` for the rest). Every value fits a byte.
pub mod kart {
    pub const BACKSPACE: u8 = 8;
    pub const TAB: u8 = 9;
    pub const ENTER: u8 = 13;
    pub const ESCAPE: u8 = 27;
    pub const SPACE: u8 = 32;
    pub const MINUS: u8 = 45;
    pub const EQUALS: u8 = 61;
    /// `KEY_CONSOLE`, the backquote.
    pub const CONSOLE: u8 = b'`';

    pub const NUMLOCK: u8 = 0x80 + 69;
    pub const SCROLLLOCK: u8 = 0x80 + 70;

    pub const KEYPAD7: u8 = 0x80 + 71;
    pub const KEYPAD8: u8 = 0x80 + 72;
    pub const KEYPAD9: u8 = 0x80 + 73;
    pub const MINUSPAD: u8 = 0x80 + 74;
    pub const KEYPAD4: u8 = 0x80 + 75;
    pub const KEYPAD5: u8 = 0x80 + 76;
    pub const KEYPAD6: u8 = 0x80 + 77;
    pub const PLUSPAD: u8 = 0x80 + 78;
    pub const KEYPAD1: u8 = 0x80 + 79;
    pub const KEYPAD2: u8 = 0x80 + 80;
    pub const KEYPAD3: u8 = 0x80 + 81;
    pub const KEYPAD0: u8 = 0x80 + 82;
    pub const KPADDEL: u8 = 0x80 + 83;

    pub const LEFTWIN: u8 = 0x80 + 91;
    pub const RIGHTWIN: u8 = 0x80 + 92;
    pub const MENU: u8 = 0x80 + 93;

    pub const KPADSLASH: u8 = 0x80 + 100;
    pub const HOME: u8 = 0x80 + 101;
    pub const UPARROW: u8 = 0x80 + 102;
    pub const PGUP: u8 = 0x80 + 103;
    pub const LEFTARROW: u8 = 0x80 + 105;
    pub const RIGHTARROW: u8 = 0x80 + 107;
    pub const END: u8 = 0x80 + 109;
    pub const DOWNARROW: u8 = 0x80 + 110;
    pub const PGDN: u8 = 0x80 + 111;
    pub const INS: u8 = 0x80 + 112;
    pub const DEL: u8 = 0x80 + 113;

    /// `KEY_F1`..`KEY_F10` are `0x80 + 0x3b ..= 0x80 + 0x44`.
    pub const F1: u8 = 0x80 + 0x3b;
    pub const F8: u8 = 0x80 + 0x42;
    pub const F9: u8 = 0x80 + 0x43;
    pub const F10: u8 = 0x80 + 0x44;
    pub const F11: u8 = 0x80 + 0x57;
    pub const F12: u8 = 0x80 + 0x58;

    pub const PAUSE: u8 = 255;

    pub const LSHIFT: u8 = 0x80 + 54;
    pub const RSHIFT: u8 = 0x80 + 55;
    pub const LCTRL: u8 = 0x80 + 29;
    pub const RCTRL: u8 = 0x80 + 30;
    pub const LALT: u8 = 0x80 + 56;
    pub const RALT: u8 = 0x80 + 57;
    pub const CAPSLOCK: u8 = 0x80 + 58;

    /// `F1`..`F12` (`F11`/`F12` are not contiguous with `F10`).
    pub fn function(n: u32) -> Option<u8> {
        match n {
            1..=10 => Some(F1 + (n - 1) as u8),
            11 => Some(F11),
            12 => Some(F12),
            _ => None,
        }
    }
}

/// HID keyboard usages (page 7) the session path reads physically.
mod hid {
    pub const DIGIT_1: u32 = 0x1e;
    pub const DIGIT_0: u32 = 0x27;
    pub const LEFT_CTRL: u32 = 0xe0;
    pub const LEFT_SHIFT: u32 = 0xe1;
    pub const LEFT_ALT: u32 = 0xe2;
    pub const LEFT_GUI: u32 = 0xe3;
    pub const RIGHT_CTRL: u32 = 0xe4;
    pub const RIGHT_SHIFT: u32 = 0xe5;
    pub const RIGHT_ALT: u32 = 0xe6;
    pub const RIGHT_GUI: u32 = 0xe7;
}

/// X11 keysyms (`inputmap::keysym`) for the non-printing keys.
mod sym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const ENTER: u32 = 0xff0d;
    pub const PAUSE: u32 = 0xff13;
    pub const SCROLL_LOCK: u32 = 0xff14;
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
    pub const MENU: u32 = 0xff67;
    pub const NUM_LOCK: u32 = 0xff7f;
    pub const KP_ENTER: u32 = 0xff8d;
    pub const KP_MULTIPLY: u32 = 0xffaa;
    pub const KP_ADD: u32 = 0xffab;
    pub const KP_SUBTRACT: u32 = 0xffad;
    pub const KP_DECIMAL: u32 = 0xffae;
    pub const KP_DIVIDE: u32 = 0xffaf;
    pub const KP_0: u32 = 0xffb0;
    pub const KP_9: u32 = 0xffb9;
    pub const F1: u32 = 0xffbe;
    pub const F12: u32 = 0xffc9;
    pub const CAPS_LOCK: u32 = 0xffe5;
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

/// A printable ASCII character as SRB2Kart wants it: lowercase, Space
/// excluded (it has its own code, which is the same byte).
fn printable(value: u32) -> Option<u8> {
    let byte = u8::try_from(value).ok()?;
    (byte.is_ascii_graphic()).then(|| byte.to_ascii_lowercase())
}

/// A key from the `inputd` session: `code` is the HID usage, `sym` its keysym.
pub fn from_session(code: u32, sym: u32) -> Option<u8> {
    match code {
        hid::LEFT_CTRL => return Some(kart::LCTRL),
        hid::RIGHT_CTRL => return Some(kart::RCTRL),
        hid::LEFT_SHIFT => return Some(kart::LSHIFT),
        hid::RIGHT_SHIFT => return Some(kart::RSHIFT),
        hid::LEFT_ALT => return Some(kart::LALT),
        hid::RIGHT_ALT => return Some(kart::RALT),
        hid::LEFT_GUI => return Some(kart::LEFTWIN),
        hid::RIGHT_GUI => return Some(kart::RIGHTWIN),
        hid::DIGIT_1..=hid::DIGIT_0 => {
            let digit = (code - hid::DIGIT_1 + 1) % 10;
            return Some(b'0' + digit as u8);
        }
        _ => {}
    }
    Some(match sym {
        0x20 => kart::SPACE,
        sym::LEFT => kart::LEFTARROW,
        sym::RIGHT => kart::RIGHTARROW,
        sym::UP => kart::UPARROW,
        sym::DOWN => kart::DOWNARROW,
        sym::ENTER | sym::KP_ENTER => kart::ENTER,
        sym::ESCAPE => kart::ESCAPE,
        sym::TAB => kart::TAB,
        sym::BACKSPACE => kart::BACKSPACE,
        sym::PAUSE => kart::PAUSE,
        sym::HOME => kart::HOME,
        sym::END => kart::END,
        sym::PAGE_UP => kart::PGUP,
        sym::PAGE_DOWN => kart::PGDN,
        sym::INSERT => kart::INS,
        sym::DELETE => kart::DEL,
        sym::MENU => kart::MENU,
        sym::CAPS_LOCK => kart::CAPSLOCK,
        sym::NUM_LOCK => kart::NUMLOCK,
        sym::SCROLL_LOCK => kart::SCROLLLOCK,
        sym::KP_ADD => kart::PLUSPAD,
        sym::KP_SUBTRACT => kart::MINUSPAD,
        sym::KP_DIVIDE => kart::KPADSLASH,
        sym::KP_DECIMAL => kart::KPADDEL,
        sym::KP_MULTIPLY => b'*',
        sym::KP_0..=sym::KP_9 => keypad_digit(sym - sym::KP_0),
        sym::F1..=sym::F12 => kart::function(sym - sym::F1 + 1)?,
        other => printable(other)?,
    })
}

/// `KEY_KEYPAD0`..`KEY_KEYPAD9`: the pad's rows are not in digit order.
fn keypad_digit(digit: u32) -> u8 {
    const PAD: [u8; 10] = [
        kart::KEYPAD0,
        kart::KEYPAD1,
        kart::KEYPAD2,
        kart::KEYPAD3,
        kart::KEYPAD4,
        kart::KEYPAD5,
        kart::KEYPAD6,
        kart::KEYPAD7,
        kart::KEYPAD8,
        kart::KEYPAD9,
    ];
    PAD[digit as usize % 10]
}

/// A key from the compositor's legacy `KeyDown`/`KeyUp`; modifier bits in the
/// top byte are ignored.
pub fn from_legacy(key: u32) -> Option<u8> {
    let code = key & legacy::CODE_MASK;
    Some(match code {
        legacy::SPACE => kart::SPACE,
        legacy::LEFT => kart::LEFTARROW,
        legacy::RIGHT => kart::RIGHTARROW,
        legacy::UP => kart::UPARROW,
        legacy::DOWN => kart::DOWNARROW,
        legacy::ENTER => kart::ENTER,
        legacy::ESCAPE => kart::ESCAPE,
        legacy::TAB => kart::TAB,
        legacy::BACKSPACE => kart::BACKSPACE,
        legacy::HOME => kart::HOME,
        legacy::END => kart::END,
        legacy::PAGE_UP => kart::PGUP,
        legacy::PAGE_DOWN => kart::PGDN,
        legacy::INSERT => kart::INS,
        legacy::DELETE => kart::DEL,
        // Never forwarded today; mapped in case a compositor starts to.
        legacy::CTRL => kart::LCTRL,
        legacy::SHIFT => kart::LSHIFT,
        legacy::ALT => kart::LALT,
        legacy::F1..=legacy::F12 => kart::function(code - legacy::F1 + 1)?,
        _ => printable(code)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values `keys.h` (Kart-Public v1.6) spells out, written as the
    /// header does so a slip in the consts above shows.
    #[test]
    fn constants_match_keys_h() {
        assert_eq!(kart::ENTER, 13);
        assert_eq!(kart::ESCAPE, 27);
        assert_eq!(kart::UPARROW, 0x80 + 102);
        assert_eq!(kart::LEFTARROW, 0x80 + 105);
        assert_eq!(kart::RIGHTARROW, 0x80 + 107);
        assert_eq!(kart::DOWNARROW, 0x80 + 110);
        assert_eq!(kart::LSHIFT, 0x80 + 54);
        assert_eq!(kart::LCTRL, 0x80 + 29);
        assert_eq!(kart::LALT, 0x80 + 56);
        assert_eq!(kart::PAUSE, 255);
        assert_eq!(kart::F11, 0x80 + 0x57);
        assert_eq!(kart::KEYPAD0, 0x80 + 82);
    }

    #[test]
    fn session_modifiers_are_real_keys() {
        assert_eq!(from_session(0xe0, 0xffe3), Some(kart::LCTRL));
        assert_eq!(from_session(0xe4, 0xffe4), Some(kart::RCTRL));
        assert_eq!(from_session(0xe1, 0xffe1), Some(kart::LSHIFT));
        assert_eq!(from_session(0xe5, 0xffe2), Some(kart::RSHIFT));
        assert_eq!(from_session(0xe2, 0xffe9), Some(kart::LALT));
        assert_eq!(from_session(0xe6, 0xffea), Some(kart::RALT));
        assert_eq!(from_session(0xe3, 0xffeb), Some(kart::LEFTWIN));
    }

    #[test]
    fn session_driving_keys() {
        assert_eq!(from_session(0x52, sym::UP), Some(kart::UPARROW));
        assert_eq!(from_session(0x51, sym::DOWN), Some(kart::DOWNARROW));
        assert_eq!(from_session(0x50, sym::LEFT), Some(kart::LEFTARROW));
        assert_eq!(from_session(0x4f, sym::RIGHT), Some(kart::RIGHTARROW));
        assert_eq!(from_session(0x04, b'a' as u32), Some(b'a'));
        assert_eq!(from_session(0x16, b's' as u32), Some(b's'));
        assert_eq!(from_session(0x07, b'd' as u32), Some(b'd'));
        assert_eq!(from_session(0x2c, 0x20), Some(kart::SPACE));
    }

    #[test]
    fn session_menu_and_misc_keys() {
        assert_eq!(from_session(0x28, sym::ENTER), Some(13));
        assert_eq!(from_session(0x58, sym::KP_ENTER), Some(13));
        assert_eq!(from_session(0x29, sym::ESCAPE), Some(27));
        assert_eq!(from_session(0x2b, sym::TAB), Some(9));
        assert_eq!(from_session(0x2a, sym::BACKSPACE), Some(8));
        assert_eq!(from_session(0x48, sym::PAUSE), Some(255));
        assert_eq!(from_session(0x35, b'`' as u32), Some(b'`'));
        assert_eq!(from_session(0x34, b'\'' as u32), Some(b'\''));
        assert_eq!(from_session(0x4a, sym::HOME), Some(0x80 + 101));
        assert_eq!(from_session(0x4d, sym::END), Some(0x80 + 109));
        assert_eq!(from_session(0x4b, sym::PAGE_UP), Some(0x80 + 103));
        assert_eq!(from_session(0x4e, sym::PAGE_DOWN), Some(0x80 + 111));
        assert_eq!(from_session(0x49, sym::INSERT), Some(0x80 + 112));
        assert_eq!(from_session(0x4c, sym::DELETE), Some(0x80 + 113));
        assert_eq!(from_session(0x39, sym::CAPS_LOCK), Some(0x80 + 58));
    }

    #[test]
    fn session_letters_follow_the_layout_and_are_lowercase() {
        // AZERTY: the key at QWERTY's `q` types `a`.
        assert_eq!(from_session(0x14, b'a' as u32), Some(b'a'));
        assert_eq!(from_session(0x1c, b'Y' as u32), Some(b'y'));
    }

    #[test]
    fn session_digits_are_physical() {
        // AZERTY's unshifted `1` key types `&`; it must still be `1`.
        assert_eq!(from_session(0x1e, b'&' as u32), Some(b'1'));
        assert_eq!(from_session(0x26, b'_' as u32), Some(b'9'));
        assert_eq!(from_session(0x27, 0xe0), Some(b'0'));
    }

    #[test]
    fn keypad_keysyms() {
        assert_eq!(from_session(0x62, sym::KP_0), Some(0x80 + 82));
        assert_eq!(from_session(0x59, sym::KP_0 + 1), Some(0x80 + 79));
        assert_eq!(from_session(0x5f, sym::KP_0 + 7), Some(0x80 + 71));
        assert_eq!(from_session(0x61, sym::KP_9), Some(0x80 + 73));
        assert_eq!(from_session(0x57, sym::KP_ADD), Some(0x80 + 78));
        assert_eq!(from_session(0x56, sym::KP_SUBTRACT), Some(0x80 + 74));
        assert_eq!(from_session(0x54, sym::KP_DIVIDE), Some(0x80 + 100));
    }

    #[test]
    fn function_keys_skip_to_f11() {
        assert_eq!(from_session(0x3a, sym::F1), Some(0x80 + 0x3b));
        assert_eq!(from_session(0x41, sym::F1 + 7), Some(kart::F8));
        assert_eq!(from_session(0x42, sym::F1 + 8), Some(kart::F9));
        assert_eq!(from_session(0x43, sym::F1 + 9), Some(0x80 + 0x44));
        assert_eq!(from_session(0x44, sym::F1 + 10), Some(0x80 + 0x57));
        assert_eq!(from_session(0x45, sym::F12), Some(0x80 + 0x58));
        assert_eq!(from_legacy(legacy::F1 + 1), Some(0x80 + 0x3c));
        assert_eq!(from_legacy(legacy::F12), Some(0x80 + 0x58));
        assert_eq!(kart::function(0), None);
        assert_eq!(kart::function(13), None);
    }

    #[test]
    fn unknown_session_keys_are_dropped() {
        assert_eq!(from_session(0x01, 0), None);
        assert_eq!(from_session(0x65, 0xfe03), None);
    }

    #[test]
    fn legacy_keys() {
        assert_eq!(from_legacy(b'a' as u32), Some(b'a'));
        assert_eq!(from_legacy(b'F' as u32 | (1 << 24)), Some(b'f'));
        assert_eq!(from_legacy(legacy::SPACE), Some(kart::SPACE));
        assert_eq!(from_legacy(legacy::UP | (1 << 25)), Some(kart::UPARROW));
        assert_eq!(from_legacy(legacy::LEFT), Some(kart::LEFTARROW));
        assert_eq!(from_legacy(legacy::ENTER), Some(13));
        assert_eq!(from_legacy(legacy::SHIFT), Some(kart::LSHIFT));
        assert_eq!(from_legacy(b'Y' as u32), Some(b'y'));
        assert_eq!(from_legacy(b',' as u32), Some(b','));
        assert_eq!(from_legacy(0x1ff), None);
    }
}
