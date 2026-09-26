//! Minimal PS/2 keyboard driver using scancode set 1 (from the i8042).

use alloc::collections::VecDeque;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::Mutex;

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
}

static QUEUE: Mutex<VecDeque<Key>> = Mutex::new(VecDeque::new());
static SHIFT: AtomicBool = AtomicBool::new(false);
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

    if released {
        if code == 0x2A || code == 0x36 {
            SHIFT.store(false, Ordering::SeqCst);
        }
        return;
    }
    if !extended && (code == 0x2A || code == 0x36) {
        SHIFT.store(true, Ordering::SeqCst);
        return;
    }

    let shift = SHIFT.load(Ordering::SeqCst);
    let key = if extended {
        decode_extended(code)
    } else {
        decode(code, shift)
    };
    if let Some(key) = key {
        // `try_lock` avoids any chance of deadlocking against a lock held on
        // the interrupted stack.
        if let Some(mut queue) = QUEUE.try_lock() {
            queue.push_back(key);
        }
    }
}

/// Block until a key is available (interrupts must be enabled).
pub fn read_key() -> Key {
    loop {
        let key = x86_64::instructions::interrupts::without_interrupts(|| QUEUE.lock().pop_front());
        if let Some(key) = key {
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
        0x1C => Key::Enter,
        _ => return None,
    })
}

fn decode(code: u8, shift: bool) -> Option<Key> {
    let key = match code {
        0x01 => Key::Escape,
        0x0E => Key::Backspace,
        0x0F => Key::Tab,
        0x1C => Key::Enter,
        0x39 => Key::Space,
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
    if shift {
        Key::Char(lower.to_ascii_uppercase())
    } else {
        Key::Char(lower)
    }
}
