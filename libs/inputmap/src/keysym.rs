//! Keysyms: what a key means under the active layout.
//!
//! Character keys report their Unicode scalar (Latin-1 for the built-in
//! layouts, which is also what X11 uses). Everything else uses the well-known
//! X11 `0xFFxx` values, so the numbers are recognisable and a later
//! `/dev/input` or XKB bridge does not need a translation table of its own.

pub const BACKSPACE: u32 = 0xFF08;
pub const TAB: u32 = 0xFF09;
pub const ENTER: u32 = 0xFF0D;
pub const PAUSE: u32 = 0xFF13;
pub const SCROLL_LOCK: u32 = 0xFF14;
pub const ESCAPE: u32 = 0xFF1B;
pub const HOME: u32 = 0xFF50;
pub const LEFT: u32 = 0xFF51;
pub const UP: u32 = 0xFF52;
pub const RIGHT: u32 = 0xFF53;
pub const DOWN: u32 = 0xFF54;
pub const PAGE_UP: u32 = 0xFF55;
pub const PAGE_DOWN: u32 = 0xFF56;
pub const END: u32 = 0xFF57;
pub const PRINT_SCREEN: u32 = 0xFF61;
pub const INSERT: u32 = 0xFF63;
pub const MENU: u32 = 0xFF67;
pub const NUM_LOCK: u32 = 0xFF7F;
pub const KP_ENTER: u32 = 0xFF8D;
pub const KP_BEGIN: u32 = 0xFF9D;
pub const KP_MULTIPLY: u32 = 0xFFAA;
pub const KP_ADD: u32 = 0xFFAB;
pub const KP_SUBTRACT: u32 = 0xFFAD;
pub const KP_DECIMAL: u32 = 0xFFAE;
pub const KP_DIVIDE: u32 = 0xFFAF;
/// `KP_0`; keypad digit `n` is `KP_0 + n`.
pub const KP_0: u32 = 0xFFB0;
/// `F1`; function key `n` (1..=12) is `F1 + n - 1`.
pub const F1: u32 = 0xFFBE;
pub const SHIFT_L: u32 = 0xFFE1;
pub const SHIFT_R: u32 = 0xFFE2;
pub const CONTROL_L: u32 = 0xFFE3;
pub const CONTROL_R: u32 = 0xFFE4;
pub const CAPS_LOCK: u32 = 0xFFE5;
pub const ALT_L: u32 = 0xFFE9;
pub const ALT_R: u32 = 0xFFEA;
pub const SUPER_L: u32 = 0xFFEB;
pub const SUPER_R: u32 = 0xFFEC;
/// ISO level-3 shift: the AltGr key on layouts that use it.
pub const ISO_LEVEL3_SHIFT: u32 = 0xFE03;
pub const DELETE: u32 = 0xFFFF;

/// Whether `sym` is a character (as opposed to a function keysym).
pub const fn is_character(sym: u32) -> bool {
    sym != 0 && sym < 0xFE00
}
