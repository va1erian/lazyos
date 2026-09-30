//! Compiled-in keymaps: US QWERTY and French AZERTY, keyed by physical HID
//! usage. Ported from the kernel's `input/layout.rs` and `keyboard.rs`, which
//! keyed on PS/2 scancodes; the layouts are unchanged (dead keys are not
//! modelled: `^` and `¨` are literal characters, as before).

use crate::keysym;

/// A selectable layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Us,
    Fr,
}

impl Layout {
    /// The layout named `name` (`"us"`, `"fr"`; case-insensitive).
    pub fn from_name(name: &str) -> Option<Layout> {
        if name.eq_ignore_ascii_case("us") {
            Some(Layout::Us)
        } else if name.eq_ignore_ascii_case("fr") {
            Some(Layout::Fr)
        } else {
            None
        }
    }

    /// The name `from_name` accepts.
    pub const fn name(self) -> &'static str {
        match self {
            Layout::Us => "us",
            Layout::Fr => "fr",
        }
    }

    /// Whether right Alt selects the third (AltGr) level rather than acting as Alt.
    pub const fn has_altgr(self) -> bool {
        matches!(self, Layout::Fr)
    }
}

/// The characters one key produces: `(plain, shifted, altgr)`; `'\0'` is
/// "nothing on this level".
pub type Levels = (char, char, char);

/// The characters of a character-producing key, `None` for every other key
/// (and for character keys the layout does not have, like the ISO key on US).
pub fn levels(layout: Layout, usage: u16) -> Option<Levels> {
    match layout {
        Layout::Us => us(usage),
        Layout::Fr => fr(usage),
    }
}

const fn pair(plain: char, shifted: char) -> Option<Levels> {
    Some((plain, shifted, '\0'))
}

/// The letter on HID usage `usage` of a US board (usages 0x04..=0x1D).
fn us_letter(usage: u16) -> Option<char> {
    (0x04..=0x1D)
        .contains(&usage)
        .then(|| (b'a' + (usage - 0x04) as u8) as char)
}

fn us(usage: u16) -> Option<Levels> {
    if let Some(letter) = us_letter(usage) {
        return pair(letter, letter.to_ascii_uppercase());
    }
    const DIGITS: [(char, char); 10] = [
        ('1', '!'),
        ('2', '@'),
        ('3', '#'),
        ('4', '$'),
        ('5', '%'),
        ('6', '^'),
        ('7', '&'),
        ('8', '*'),
        ('9', '('),
        ('0', ')'),
    ];
    if (0x1E..=0x27).contains(&usage) {
        let (plain, shifted) = DIGITS[(usage - 0x1E) as usize];
        return pair(plain, shifted);
    }
    match usage {
        0x2C => pair(' ', ' '),
        0x2D => pair('-', '_'),
        0x2E => pair('=', '+'),
        0x2F => pair('[', '{'),
        0x30 => pair(']', '}'),
        0x31 => pair('\\', '|'),
        0x33 => pair(';', ':'),
        0x34 => pair('\'', '"'),
        0x35 => pair('`', '~'),
        0x36 => pair(',', '<'),
        0x37 => pair('.', '>'),
        0x38 => pair('/', '?'),
        _ => None,
    }
}

fn fr(usage: u16) -> Option<Levels> {
    // The three swapped letters; `m` moved to the `;` key.
    if let Some(letter) = us_letter(usage) {
        let letter = match letter {
            'q' => 'a',
            'a' => 'q',
            'w' => 'z',
            'z' => 'w',
            // The QWERTY `m` key carries `,` on AZERTY.
            'm' => return pair(',', '?'),
            other => other,
        };
        return pair(letter, letter.to_ascii_uppercase());
    }
    let triple = |plain, shifted, altgr| Some((plain, shifted, altgr));
    match usage {
        0x1E => pair('&', '1'),
        0x1F => triple('é', '2', '~'),
        0x20 => triple('"', '3', '#'),
        0x21 => triple('\'', '4', '{'),
        0x22 => triple('(', '5', '['),
        0x23 => triple('-', '6', '|'),
        0x24 => triple('è', '7', '`'),
        0x25 => triple('_', '8', '\\'),
        0x26 => triple('ç', '9', '^'),
        0x27 => triple('à', '0', '@'),
        0x2C => pair(' ', ' '),
        0x2D => triple(')', '°', ']'),
        0x2E => triple('=', '+', '}'),
        0x2F => pair('^', '¨'),
        0x30 => pair('$', '£'),
        0x31 => pair('*', 'µ'),
        0x33 => pair('m', 'M'),
        0x34 => pair('ù', '%'),
        0x35 => pair('²', '³'),
        0x36 => pair(';', '.'),
        0x37 => pair(':', '/'),
        0x38 => pair('!', '§'),
        // The extra key of ISO boards, left of W (Z on QWERTY).
        0x64 => pair('<', '>'),
        _ => None,
    }
}

/// The keysym of a key that is not a character key (the same for every
/// layout), given the NumLock state for the keypad digit block.
pub fn function_sym(usage: u16, num_lock: bool) -> Option<u32> {
    use keysym::*;
    Some(match usage {
        0x28 => ENTER,
        0x29 => ESCAPE,
        0x2A => BACKSPACE,
        0x2B => TAB,
        0x39 => CAPS_LOCK,
        0x3A..=0x45 => F1 + u32::from(usage - 0x3A),
        0x46 => PRINT_SCREEN,
        0x47 => SCROLL_LOCK,
        0x48 => PAUSE,
        0x49 => INSERT,
        0x4A => HOME,
        0x4B => PAGE_UP,
        0x4C => DELETE,
        0x4D => END,
        0x4E => PAGE_DOWN,
        0x4F => RIGHT,
        0x50 => LEFT,
        0x51 => DOWN,
        0x52 => UP,
        0x53 => NUM_LOCK,
        0x54 => KP_DIVIDE,
        0x55 => KP_MULTIPLY,
        0x56 => KP_SUBTRACT,
        0x57 => KP_ADD,
        0x58 => KP_ENTER,
        0x59..=0x63 => return keypad_sym(usage, num_lock),
        0x65 => MENU,
        0xE0 => CONTROL_L,
        0xE1 => SHIFT_L,
        0xE2 => ALT_L,
        0xE3 => SUPER_L,
        0xE4 => CONTROL_R,
        0xE5 => SHIFT_R,
        0xE6 => ALT_R,
        0xE7 => SUPER_R,
        _ => return None,
    })
}

/// Keypad digit block (usages 0x59..=0x63: 1-9, 0, `.`): digits with NumLock,
/// navigation without.
fn keypad_sym(usage: u16, num_lock: bool) -> Option<u32> {
    use keysym::*;
    if num_lock {
        return Some(match usage {
            0x59..=0x61 => KP_0 + u32::from(usage - 0x59) + 1,
            0x62 => KP_0,
            _ => KP_DECIMAL,
        });
    }
    Some(match usage {
        0x59 => END,
        0x5A => DOWN,
        0x5B => PAGE_DOWN,
        0x5C => LEFT,
        0x5D => KP_BEGIN,
        0x5E => RIGHT,
        0x5F => HOME,
        0x60 => UP,
        0x61 => PAGE_UP,
        0x62 => INSERT,
        _ => DELETE,
    })
}

/// The text a keypad key types: digits and `.` with NumLock, the operators
/// always.
pub fn keypad_text(usage: u16, num_lock: bool) -> Option<char> {
    match usage {
        0x54 => Some('/'),
        0x55 => Some('*'),
        0x56 => Some('-'),
        0x57 => Some('+'),
        0x59..=0x61 if num_lock => Some((b'1' + (usage - 0x59) as u8) as char),
        0x62 if num_lock => Some('0'),
        0x63 if num_lock => Some('.'),
        _ => None,
    }
}
