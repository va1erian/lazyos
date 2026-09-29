//! Keyboard layouts: which character a printable set-1 scancode produces.
//!
//! The kernel keeps US QWERTY hard-wired in `keyboard::decode`; this module
//! adds French AZERTY on top of it. The layout is picked at build time with
//! `LAZYOS_KBD_LAYOUT=fr` (default `us`), like the other `LAZYOS_*` switches.
//! Dead keys are not modelled: `^` and `¨` are emitted as literal characters.

use core::sync::atomic::{AtomicU8, Ordering};

const US: u8 = 0;
const FR: u8 = 1;

/// Byte-wise `==` for a `const` initialiser (`str == str` is not `const`).
const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const DEFAULT: u8 = match option_env!("LAZYOS_KBD_LAYOUT") {
    Some(name) if same(name, "fr") => FR,
    _ => US,
};

static ACTIVE: AtomicU8 = AtomicU8::new(DEFAULT);

/// Whether the French AZERTY layout is active.
pub fn is_french() -> bool {
    ACTIVE.load(Ordering::Relaxed) == FR
}

/// Switch layout. Only the in-kernel suite changes it after boot.
#[cfg(lazyos_tests)]
pub fn set_french(french: bool) {
    ACTIVE.store(if french { FR } else { US }, Ordering::Relaxed);
}

/// The letter at `code` on the AZERTY letter rows (rows are contiguous
/// scancode runs, exactly as on a QWERTY board; only the letters differ).
fn letter_at(code: u8) -> Option<char> {
    const ROWS: [(u8, &str); 3] = [(0x10, "azertyuiop"), (0x1E, "qsdfghjklm"), (0x2C, "wxcvbn")];
    ROWS.iter().find_map(|&(first, letters)| {
        let index = code.checked_sub(first)? as usize;
        letters.chars().nth(index)
    })
}

/// `(plain, shifted, altgr)` for a non-letter AZERTY key; `'\0'` is unmapped.
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
        // The extra key of ISO boards, left of Z.
        0x56 => ('<', '>', '\0'),
        _ => return None,
    })
}

/// Whether `code` is a character key on the AZERTY board. `french` returns
/// `None` both for these keys on a layer that assigns them nothing (AltGr+`^`)
/// and for non-character keys (Esc, Enter); callers need the difference so an
/// unassigned key does not fall back to the US table.
pub fn is_french_character_key(code: u8) -> bool {
    letter_at(code).is_some() || symbol_at(code).is_some()
}

/// The AZERTY character for a printable scancode, `None` when the key is not
/// a character key or the layer has no character for it. Letters are returned
/// lowercase; the caller applies Shift and Ctrl.
pub fn french(code: u8, shift: bool, altgr: bool) -> Option<char> {
    if let Some(letter) = letter_at(code) {
        return Some(letter);
    }
    let (plain, shifted, alt) = symbol_at(code)?;
    let ch = if altgr {
        alt
    } else if shift {
        shifted
    } else {
        plain
    };
    (ch != '\0').then_some(ch)
}
