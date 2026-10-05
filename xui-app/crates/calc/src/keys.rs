//! The keyboard: which key presses which calculator key.
//!
//! xui's window-level hook (`Ui::on_key`) sees virtual keys, not the
//! characters they type, so the symbols are read by their US key positions
//! with Shift (`Shift+8` is `*`, `Shift+=` is `+`), as LazyOS reports them;
//! the keypad's own codes are accepted too.

use xui_core::{Key, Modifiers};

use crate::app::Msg;
use crate::engine::{Op, Press};

/// `VK_OEM_PLUS`: `=` and, shifted, `+`.
const OEM_PLUS: u16 = 0xBB;
/// `VK_OEM_COMMA`: a decimal comma.
const OEM_COMMA: u16 = 0xBC;
/// `VK_OEM_MINUS`: `-`.
const OEM_MINUS: u16 = 0xBD;
/// `VK_OEM_PERIOD`: `.`.
const OEM_PERIOD: u16 = 0xBE;
/// `VK_OEM_2`: `/`.
const OEM_SLASH: u16 = 0xBF;
/// `VK_NUMPAD0..9`, then the keypad operators.
const NUMPAD0: u16 = 0x60;
const NUMPAD9: u16 = 0x69;
const MULTIPLY: u16 = 0x6A;
const ADD: u16 = 0x6B;
const SUBTRACT: u16 = 0x6D;
const DECIMAL: u16 = 0x6E;
const DIVIDE: u16 = 0x6F;

/// The message for `key`, or `None` for a key the calculator leaves alone
/// (Tab still moves the focus, a Ctrl or Alt chord stays a command).
pub fn shortcut(key: Key, modifiers: Modifiers) -> Option<Msg> {
    if modifiers.ctrl || modifiers.alt || modifiers.win {
        return None;
    }
    if key == Key::Q {
        return Some(Msg::Quit);
    }
    press(key, modifiers.shift).map(Msg::Press)
}

fn press(key: Key, shift: bool) -> Option<Press> {
    let code = key.code();
    let press = match code {
        0x38 if shift => Press::Op(Op::Mul),
        0x35 if shift => Press::Percent,
        0x30..=0x39 if !shift => Press::Digit((code - 0x30) as u8),
        NUMPAD0..=NUMPAD9 => Press::Digit((code - NUMPAD0) as u8),
        OEM_PLUS if shift => Press::Op(Op::Add),
        OEM_PLUS => Press::Equals,
        OEM_MINUS if !shift => Press::Op(Op::Sub),
        OEM_SLASH if !shift => Press::Op(Op::Div),
        OEM_PERIOD | OEM_COMMA if !shift => Press::Point,
        MULTIPLY => Press::Op(Op::Mul),
        ADD => Press::Op(Op::Add),
        SUBTRACT => Press::Op(Op::Sub),
        DIVIDE => Press::Op(Op::Div),
        DECIMAL => Press::Point,
        _ if key == Key::RETURN => Press::Equals,
        _ if key == Key::BACK => Press::Backspace,
        _ if key == Key::ESCAPE => Press::AllClear,
        _ if key == Key::DELETE || key == Key::C => Press::Clear,
        _ if key == Key::N => Press::Negate,
        _ => return None,
    };
    Some(press)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: Modifiers = Modifiers::NONE;
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..Modifiers::NONE
    };

    fn pressed(code: u16, modifiers: Modifiers) -> Option<Press> {
        match shortcut(Key::from_code(code), modifiers) {
            Some(Msg::Press(press)) => Some(press),
            _ => None,
        }
    }

    #[test]
    fn digits_on_the_top_row_and_the_keypad() {
        for digit in 0..=9u8 {
            let want = Some(Press::Digit(digit));
            assert_eq!(pressed(0x30 + u16::from(digit), PLAIN), want);
            assert_eq!(pressed(NUMPAD0 + u16::from(digit), PLAIN), want);
        }
    }

    #[test]
    fn operators_by_their_us_keys() {
        assert_eq!(pressed(OEM_PLUS, SHIFT), Some(Press::Op(Op::Add)));
        assert_eq!(pressed(OEM_MINUS, PLAIN), Some(Press::Op(Op::Sub)));
        assert_eq!(pressed(0x38, SHIFT), Some(Press::Op(Op::Mul)));
        assert_eq!(pressed(OEM_SLASH, PLAIN), Some(Press::Op(Op::Div)));
        assert_eq!(pressed(0x35, SHIFT), Some(Press::Percent));
        assert_eq!(pressed(OEM_PLUS, PLAIN), Some(Press::Equals));
        assert_eq!(pressed(OEM_PERIOD, PLAIN), Some(Press::Point));
        assert_eq!(pressed(OEM_COMMA, PLAIN), Some(Press::Point));
    }

    #[test]
    fn keypad_operators() {
        assert_eq!(pressed(ADD, PLAIN), Some(Press::Op(Op::Add)));
        assert_eq!(pressed(SUBTRACT, PLAIN), Some(Press::Op(Op::Sub)));
        assert_eq!(pressed(MULTIPLY, PLAIN), Some(Press::Op(Op::Mul)));
        assert_eq!(pressed(DIVIDE, PLAIN), Some(Press::Op(Op::Div)));
        assert_eq!(pressed(DECIMAL, PLAIN), Some(Press::Point));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(pressed(Key::RETURN.code(), PLAIN), Some(Press::Equals));
        assert_eq!(pressed(Key::BACK.code(), PLAIN), Some(Press::Backspace));
        assert_eq!(pressed(Key::ESCAPE.code(), PLAIN), Some(Press::AllClear));
        assert_eq!(pressed(Key::DELETE.code(), PLAIN), Some(Press::Clear));
        assert_eq!(pressed(Key::C.code(), PLAIN), Some(Press::Clear));
        assert_eq!(pressed(Key::N.code(), PLAIN), Some(Press::Negate));
    }

    #[test]
    fn shifted_symbols_are_not_digits() {
        // Shift+1 is `!`, Shift+- is `_`: nothing on a calculator.
        assert_eq!(pressed(0x31, SHIFT), None);
        assert_eq!(pressed(OEM_MINUS, SHIFT), None);
        assert_eq!(pressed(OEM_SLASH, SHIFT), None);
    }

    #[test]
    fn chords_and_other_keys_are_left_alone() {
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        assert!(shortcut(Key::C, ctrl).is_none());
        assert!(shortcut(Key::from_code(0x31), ctrl).is_none());
        assert!(shortcut(Key::TAB, PLAIN).is_none());
        assert!(shortcut(Key::A, PLAIN).is_none());
        assert!(matches!(shortcut(Key::Q, PLAIN), Some(Msg::Quit)));
    }
}
