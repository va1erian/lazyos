//! Keyboard layouts (French AZERTY next to the built-in US QWERTY).

use super::*;
use crate::input::keyboard::{self, Key};
use crate::input::layout;

/// Run `body` under one layout with clean modifier state, restoring US after.
fn with_layout(french: bool, body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    keyboard::reset();
    layout::set_french(french);
    let result = body();
    layout::set_french(false);
    keyboard::reset();
    result
}

fn press_altgr(down: bool) {
    keyboard::push_scancode(0xE0);
    keyboard::push_scancode(if down { 0x38 } else { 0xB8 });
}

fn expect(code: u8, shift: bool, want: Key) -> Result<(), String> {
    let got = keyboard::decode_for_test(code, shift);
    check!(
        got == Some(want),
        "scancode {code:#x} shift={shift}: got {got:?}, want {want:?}"
    );
    Ok(())
}

fn french_letters_and_symbols() -> Result<(), String> {
    with_layout(true, || {
        // The three swapped letters and the relocated `m`.
        expect(0x10, false, Key::Char('a'))?;
        expect(0x10, true, Key::Char('A'))?;
        expect(0x11, false, Key::Char('z'))?;
        expect(0x1E, false, Key::Char('q'))?;
        expect(0x27, false, Key::Char('m'))?;
        expect(0x2C, false, Key::Char('w'))?;
        // Digit row: symbols and accents plain, digits shifted.
        expect(0x02, false, Key::Char('&'))?;
        expect(0x02, true, Key::Char('1'))?;
        expect(0x03, false, Key::Char('é'))?;
        expect(0x08, false, Key::Char('è'))?;
        expect(0x0A, false, Key::Char('ç'))?;
        expect(0x0B, false, Key::Char('à'))?;
        expect(0x28, false, Key::Char('ù'))?;
        expect(0x35, false, Key::Char('!'))?;
        expect(0x32, false, Key::Char(','))?;
        expect(0x56, true, Key::Char('>'))?;
        // Non-character keys are layout-independent.
        expect(0x1C, false, Key::Enter)?;
        expect(0x39, false, Key::Space)?;
        expect(0x0E, false, Key::Backspace)
    })
}

fn french_altgr_layer() -> Result<(), String> {
    with_layout(true, || {
        press_altgr(true);
        expect(0x0B, false, Key::Char('@'))?;
        expect(0x05, false, Key::Char('{'))?;
        expect(0x0C, false, Key::Char(']'))?;
        expect(0x09, false, Key::Char('\\'))?;
        press_altgr(false);
        expect(0x0B, false, Key::Char('à'))
    })
}

fn french_ctrl_letter_uses_physical_key() -> Result<(), String> {
    with_layout(true, || {
        keyboard::push_scancode(0x1D); // Ctrl down
                                       // The key at QWERTY-Q reads `a` on AZERTY, so Ctrl+it is ^A.
        expect(0x10, false, Key::Char('\u{1}'))
    })
}

fn us_layout_is_unchanged() -> Result<(), String> {
    with_layout(false, || {
        expect(0x10, false, Key::Char('q'))?;
        expect(0x1E, false, Key::Char('a'))?;
        expect(0x02, false, Key::Char('1'))?;
        // AltGr is a French concept: on US the key must not select a layer.
        press_altgr(true);
        expect(0x0B, false, Key::Char('0'))
    })
}

/// Sustained decode over every scancode, modifier combination and layout:
/// decoding is deterministic, never panics, and every character it produces
/// is one the Latin-1 font atlas can draw.
fn layout_decode_soak() -> Result<(), String> {
    const ROUNDS: usize = 300;
    for french in [false, true] {
        with_layout(french, || {
            for round in 0..ROUNDS {
                press_altgr(round % 2 == 0);
                for code in 0..0x80u8 {
                    for shift in [false, true] {
                        let first = keyboard::decode_for_test(code, shift);
                        check!(
                            first == keyboard::decode_for_test(code, shift),
                            "scancode {code:#x} decoded differently twice"
                        );
                        if let Some(Key::Char(c)) = first {
                            check!(
                                (c as u32) <= 0xFF,
                                "scancode {code:#x} produced {c:?}, outside Latin-1"
                            );
                        }
                    }
                }
            }
            Ok(())
        })?;
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("kbd_french_letters_and_symbols", french_letters_and_symbols),
    ("kbd_french_altgr_layer", french_altgr_layer),
    (
        "kbd_french_ctrl_letter_uses_physical_key",
        french_ctrl_letter_uses_physical_key,
    ),
    ("kbd_us_layout_is_unchanged", us_layout_is_unchanged),
    ("kbd_layout_decode_soak", layout_decode_soak),
];
