//! The compiled-in layouts, checked against the kernel's original tables.

use super::*;
use crate::keymap::{self, Layout};

#[test]
fn us_letters_digits_and_symbols() {
    let mut rig = Rig::new(Layout::Us);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("a"));
    assert_eq!(typed(&mut rig, Q).as_deref(), Some("q"));
    assert_eq!(typed(&mut rig, ONE).as_deref(), Some("1"));
    assert_eq!(typed(&mut rig, SPACE).as_deref(), Some(" "));
    rig.down(LSHIFT);
    assert_eq!(typed(&mut rig, A).as_deref(), Some("A"));
    assert_eq!(typed(&mut rig, ONE).as_deref(), Some("!"));
    assert_eq!(typed(&mut rig, 0x33).as_deref(), Some(":"));
    rig.up(LSHIFT);
    // AltGr is a French concept: right Alt is Alt on US and types nothing.
    rig.down(RALT);
    assert_eq!(typed(&mut rig, 0x27), None);
}

#[test]
fn french_matches_the_kernels_original_tables() {
    // The kernel's scancode-keyed AZERTY data, verbatim.
    fn letter_at(code: u8) -> Option<char> {
        const ROWS: [(u8, &str); 3] =
            [(0x10, "azertyuiop"), (0x1E, "qsdfghjklm"), (0x2C, "wxcvbn")];
        ROWS.iter().find_map(|&(first, letters)| {
            let index = code.checked_sub(first)? as usize;
            letters.chars().nth(index)
        })
    }
    fn symbol_at(code: u8) -> Option<(char, char, char)> {
        Some(match code {
            0x02 => ('&', '1', '\0'),
            0x03 => ('é', '2', '~'),
            0x04 => ('"', '3', '#'),
            0x05 => ('\'', '4', '{'),
            0x06 => ('(', '5', '['),
            0x07 => ('-', '6', '|'),
            0x08 => ('è', '7', '`'),
            0x09 => ('_', '8', '\\'),
            0x0A => ('ç', '9', '^'),
            0x0B => ('à', '0', '@'),
            0x0C => (')', '°', ']'),
            0x0D => ('=', '+', '}'),
            0x1A => ('^', '¨', '\0'),
            0x1B => ('$', '£', '\0'),
            0x28 => ('ù', '%', '\0'),
            0x29 => ('²', '³', '\0'),
            0x2B => ('*', 'µ', '\0'),
            0x32 => (',', '?', '\0'),
            0x33 => (';', '.', '\0'),
            0x34 => (':', '/', '\0'),
            0x35 => ('!', '§', '\0'),
            0x56 => ('<', '>', '\0'),
            _ => return None,
        })
    }
    let mut compared = 0;
    for code in 0x01..0x59u8 {
        let Some(usage) = kernel_hid::translate(false, code) else {
            continue;
        };
        let new = keymap::levels(Layout::Fr, usage);
        let old = match (letter_at(code), symbol_at(code)) {
            (Some(letter), _) => Some((letter, letter.to_ascii_uppercase(), '\0')),
            // Space is not layout-specific: the kernel's shared table typed it.
            (None, _) if code == 0x39 => Some((' ', ' ', '\0')),
            (None, sym) => sym,
        };
        assert_eq!(new, old, "scancode {code:#x} usage {usage:#x}");
        compared += (old.is_some()) as usize;
    }
    // 26 letters, the digit row, and every symbol key.
    assert!(compared >= 46, "only {compared} keys compared");
}

#[test]
fn us_matches_the_kernels_original_table() {
    // The kernel's US decode, as (scancode, plain, shifted).
    let table: &[(u8, char, char)] = &[
        (0x02, '1', '!'),
        (0x03, '2', '@'),
        (0x04, '3', '#'),
        (0x05, '4', '$'),
        (0x06, '5', '%'),
        (0x07, '6', '^'),
        (0x08, '7', '&'),
        (0x09, '8', '*'),
        (0x0A, '9', '('),
        (0x0B, '0', ')'),
        (0x0C, '-', '_'),
        (0x0D, '=', '+'),
        (0x1A, '[', '{'),
        (0x1B, ']', '}'),
        (0x27, ';', ':'),
        (0x28, '\'', '"'),
        (0x29, '`', '~'),
        (0x2B, '\\', '|'),
        (0x33, ',', '<'),
        (0x34, '.', '>'),
        (0x35, '/', '?'),
        (0x39, ' ', ' '),
    ];
    for &(code, plain, shifted) in table {
        let usage = kernel_hid::translate(false, code).unwrap();
        assert_eq!(
            keymap::levels(Layout::Us, usage),
            Some((plain, shifted, '\0')),
            "scancode {code:#x}"
        );
    }
    // Letters: QWERTY rows of set-1 scancodes.
    for (first, letters) in [
        (0x10u8, "qwertyuiop"),
        (0x1E, "asdfghjkl"),
        (0x2C, "zxcvbnm"),
    ] {
        for (index, letter) in letters.chars().enumerate() {
            let usage = kernel_hid::translate(false, first + index as u8).unwrap();
            assert_eq!(
                keymap::levels(Layout::Us, usage),
                Some((letter, letter.to_ascii_uppercase(), '\0'))
            );
        }
    }
    // ANSI boards have no ISO key.
    assert_eq!(keymap::levels(Layout::Us, 0x64), None);
}
