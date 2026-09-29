//! Minimal PS/2 keyboard driver using scancode set 1 (from the i8042).

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

use super::layout;
use crate::display;

/// A decoded key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Space,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    /// Modifier press/release (issue #167): forwarded to a bound compositor,
    /// which uses them for global hotkeys; the kernel terminal consumes them.
    Shift,
    Ctrl,
    Alt,
    Super,
    /// Function keys are compositor-only for now (Alt+F4 closes a window).
    F4,
}

/// A modifier tracked per physical key (issue #175): left/right Shift, Ctrl,
/// Alt and the two Super keys each set their own flag, so releasing one of
/// two held keys (e.g. right Alt while left Alt is still down) does not drop
/// the aggregate state and forward a spurious key-up.
struct ModifierPair {
    left: AtomicBool,
    right: AtomicBool,
}

impl ModifierPair {
    const fn new() -> ModifierPair {
        ModifierPair {
            left: AtomicBool::new(false),
            right: AtomicBool::new(false),
        }
    }

    /// Whether either physical key is currently held.
    fn held(&self) -> bool {
        self.left.load(Ordering::SeqCst) || self.right.load(Ordering::SeqCst)
    }

    /// Record a physical press/release. Returns the new aggregate state only
    /// when it actually changed, so a second press while the other side is
    /// still down, or a re-sent auto-repeat make code, reports no transition.
    fn set(&self, right: bool, pressed: bool) -> Option<bool> {
        let before = self.held();
        let side = if right { &self.right } else { &self.left };
        side.store(pressed, Ordering::SeqCst);
        let after = self.held();
        (before != after).then_some(after)
    }

    /// Only the `#[cfg(lazyos_tests)]` harness hook below calls this.
    #[allow(dead_code)]
    fn reset(&self) {
        self.left.store(false, Ordering::SeqCst);
        self.right.store(false, Ordering::SeqCst);
    }
}

static QUEUE: Mutex<VecDeque<Key>> = Mutex::new(VecDeque::new());
static SHIFT: ModifierPair = ModifierPair::new();
static CTRL: ModifierPair = ModifierPair::new();
static ALT: ModifierPair = ModifierPair::new();
static SUPER: ModifierPair = ModifierPair::new();
/// Right Alt as AltGr, tracked only while a layout uses it (never forwarded).
static ALTGR: ModifierPair = ModifierPair::new();
static EXTENDED: AtomicBool = AtomicBool::new(false);

/// Feed a raw scancode from the i8042 (called from the IRQ1 handler).
pub fn push_scancode(scancode: u8) {
    if scancode == 0xE0 {
        EXTENDED.store(true, Ordering::SeqCst);
        return;
    }
    let extended = EXTENDED.swap(false, Ordering::SeqCst);
    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;

    // With AZERTY, right Alt is AltGr: it selects a character layer and is not
    // an Alt for the compositor's hotkeys.
    if extended && code == 0x38 && layout::is_french() {
        ALTGR.set(true, !released);
        return;
    }

    // Modifier keys update their tracked per-key state and, while a
    // compositor is bound, are forwarded as modifier key codes only on a real
    // aggregate transition (issue #175: two physical keys share one logical
    // modifier, and auto-repeat must not re-send a key-down). They are never
    // routed to a task: the kernel terminal consumes them exactly as before.
    if let Some((pair, right, key)) = modifier(code, extended) {
        if let Some(pressed) = pair.set(right, !released) {
            if display::bound() {
                display::push_key(key, pressed);
            }
        }
        return;
    }

    if released {
        // A bound compositor observes key releases too; the kernel terminal
        // only cares about presses, so this is display-only (issue #113).
        if display::bound() {
            let shift = SHIFT.held();
            let key = if extended {
                decode_extended(code)
            } else {
                decode(code, shift)
            };
            if let Some(key) = key {
                display::push_key(key, false);
            }
        }
        return;
    }

    let shift = SHIFT.held();
    let key = if extended {
        decode_extended(code)
    } else {
        decode(code, shift)
    };
    if let Some(key) = key {
        // A bound compositor receives the raw key; otherwise route it to the
        // focused task (switching focus on Tab) as before. Function keys are
        // compositor-only: the terminal mapping would turn them into a NUL.
        if display::bound() {
            display::push_key(key, true);
        } else if key != Key::F4 {
            crate::task::on_key(key);
        }
    }
}

/// The modifier a scancode denotes, ignoring its release bit, plus which
/// physical side it is (`true` = right) and the logical key it forwards.
/// Left/right shift, ctrl, alt, and the two Super keys each track their own
/// side of one pair (issue #175).
fn modifier(code: u8, extended: bool) -> Option<(&'static ModifierPair, bool, Key)> {
    Some(match (extended, code) {
        (false, 0x2A) => (&SHIFT, false, Key::Shift),
        (false, 0x36) => (&SHIFT, true, Key::Shift),
        (false, 0x1D) => (&CTRL, false, Key::Ctrl),
        (true, 0x1D) => (&CTRL, true, Key::Ctrl),
        (false, 0x38) => (&ALT, false, Key::Alt),
        (true, 0x38) => (&ALT, true, Key::Alt),
        (true, 0x5B) => (&SUPER, false, Key::Super),
        (true, 0x5C) => (&SUPER, true, Key::Super),
        _ => return None,
    })
}
/// Non-blocking: return the next key if one is queued.
pub fn try_read_key() -> Option<Key> {
    x86_64::instructions::interrupts::without_interrupts(|| QUEUE.lock().pop_front())
}

/// Block until a key is available (interrupts must be enabled).
#[allow(dead_code)]
pub fn read_key() -> Key {
    loop {
        if let Some(key) = try_read_key() {
            return key;
        }
        x86_64::instructions::hlt();
    }
}

fn decode_extended(code: u8) -> Option<Key> {
    Some(match code {
        0x48 => Key::Up,
        0x50 => Key::Down,
        0x4B => Key::Left,
        0x4D => Key::Right,
        0x49 => Key::PageUp,
        0x51 => Key::PageDown,
        0x47 => Key::Home,
        0x4F => Key::End,
        0x1C => Key::Enter,
        _ => return None,
    })
}

fn decode(code: u8, shift: bool) -> Option<Key> {
    if layout::is_french() {
        if let Some(ch) = layout::french(code, shift, ALTGR.right.load(Ordering::SeqCst)) {
            // Letters keep their Shift and Ctrl behaviour; symbols are final.
            return Some(if ch.is_ascii_lowercase() {
                letter(shift, ch)
            } else {
                Key::Char(ch)
            });
        }
    }
    let key = match code {
        0x01 => Key::Escape,
        0x0E => Key::Backspace,
        0x0F => Key::Tab,
        0x1C => Key::Enter,
        0x39 => Key::Space,
        0x3E => Key::F4,
        // Digits 1..9, 0
        0x02 => Key::Char(shifted(shift, '1', '!')),
        0x03 => Key::Char(shifted(shift, '2', '@')),
        0x04 => Key::Char(shifted(shift, '3', '#')),
        0x05 => Key::Char(shifted(shift, '4', '$')),
        0x06 => Key::Char(shifted(shift, '5', '%')),
        0x07 => Key::Char(shifted(shift, '6', '^')),
        0x08 => Key::Char(shifted(shift, '7', '&')),
        0x09 => Key::Char(shifted(shift, '8', '*')),
        0x0A => Key::Char(shifted(shift, '9', '(')),
        0x0B => Key::Char(shifted(shift, '0', ')')),
        0x0C => Key::Char(shifted(shift, '-', '_')),
        0x0D => Key::Char(shifted(shift, '=', '+')),
        0x1A => Key::Char(shifted(shift, '[', '{')),
        0x1B => Key::Char(shifted(shift, ']', '}')),
        0x27 => Key::Char(shifted(shift, ';', ':')),
        0x28 => Key::Char(shifted(shift, '\'', '"')),
        0x29 => Key::Char(shifted(shift, '`', '~')),
        0x2B => Key::Char(shifted(shift, '\\', '|')),
        0x33 => Key::Char(shifted(shift, ',', '<')),
        0x34 => Key::Char(shifted(shift, '.', '>')),
        0x35 => Key::Char(shifted(shift, '/', '?')),
        // Letters (QWERTY row mapping of set-1 scancodes).
        0x10 => letter(shift, 'q'),
        0x11 => letter(shift, 'w'),
        0x12 => letter(shift, 'e'),
        0x13 => letter(shift, 'r'),
        0x14 => letter(shift, 't'),
        0x15 => letter(shift, 'y'),
        0x16 => letter(shift, 'u'),
        0x17 => letter(shift, 'i'),
        0x18 => letter(shift, 'o'),
        0x19 => letter(shift, 'p'),
        0x1E => letter(shift, 'a'),
        0x1F => letter(shift, 's'),
        0x20 => letter(shift, 'd'),
        0x21 => letter(shift, 'f'),
        0x22 => letter(shift, 'g'),
        0x23 => letter(shift, 'h'),
        0x24 => letter(shift, 'j'),
        0x25 => letter(shift, 'k'),
        0x26 => letter(shift, 'l'),
        0x2C => letter(shift, 'z'),
        0x2D => letter(shift, 'x'),
        0x2E => letter(shift, 'c'),
        0x2F => letter(shift, 'v'),
        0x30 => letter(shift, 'b'),
        0x31 => letter(shift, 'n'),
        0x32 => letter(shift, 'm'),
        _ => return None,
    };
    Some(key)
}

fn shifted(shift: bool, normal: char, shifted: char) -> char {
    if shift {
        shifted
    } else {
        normal
    }
}

fn letter(shift: bool, lower: char) -> Key {
    // Ctrl+letter is the corresponding C0 control character (Ctrl-C -> ETX),
    // so the terminal layer (`task::on_key`) can tell it from a plain letter.
    if CTRL.held() {
        Key::Char(((lower as u8) & 0x1f) as char)
    } else if shift {
        Key::Char(lower.to_ascii_uppercase())
    } else {
        Key::Char(lower)
    }
}

/// Test-harness hook: forget every held modifier and queued key so suites do
/// not leak keyboard state into each other.
#[cfg(lazyos_tests)]
pub fn reset() {
    SHIFT.reset();
    CTRL.reset();
    ALT.reset();
    SUPER.reset();
    ALTGR.reset();
    EXTENDED.store(false, Ordering::SeqCst);
    QUEUE.lock().clear();
}

/// Test-harness hook: decode `code` under the current layout and modifiers.
#[cfg(lazyos_tests)]
pub fn decode_for_test(code: u8, shift: bool) -> Option<Key> {
    decode(code, shift)
}
