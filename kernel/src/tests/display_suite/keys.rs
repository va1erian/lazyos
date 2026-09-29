//! Navigation, editing and function keys reaching a bound compositor with
//! the codes `docs/architecture/display.md` promises to xui clients.

use super::*;
use crate::display::key;

/// Bind a scratch task as the compositor, drain the seeded pointer event.
fn bind_compositor() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    scratch_task()?;
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    drain()?;
    Ok(())
}

/// Poll every queued event as `(kind, a)`.
fn drain() -> Result<Vec<(u32, i32)>, String> {
    let mut events = [0u8; 16 * 64];
    let count = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        events.as_mut_ptr() as u64,
        events.len() as u64,
    ) as usize;
    check!(count <= 64, "poll returned {count}");
    Ok((0..count).map(|index| event_at(&events, index)).collect())
}

fn unbind() {
    let _ = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    crate::input::keyboard::reset();
    task::harness::reset();
}

/// Feed `scancodes`, run `check`, and always unbind.
fn with_compositor(
    scancodes: &[u8],
    check: impl FnOnce(&[(u32, i32)]) -> Result<(), String>,
) -> Result<(), String> {
    bind_compositor()?;
    for &scancode in scancodes {
        crate::input::keyboard::push_scancode(scancode);
    }
    let events = drain();
    unbind();
    check(&events?)
}

/// Arrows, Home/End, PgUp/PgDn, Delete, Insert and F1-F12 (down + up) arrive
/// with their documented codes; F-keys 1..=10 and 11/12 use different
/// scancode ranges.
pub fn nav_and_function_keys() -> Result<(), String> {
    // (extended, make code, display key)
    let table: &[(bool, u8, u32)] = &[
        (true, 0x4B, key::LEFT),
        (true, 0x4D, key::RIGHT),
        (true, 0x48, key::UP),
        (true, 0x50, key::DOWN),
        (true, 0x49, key::PAGE_UP),
        (true, 0x51, key::PAGE_DOWN),
        (true, 0x47, key::HOME),
        (true, 0x4F, key::END),
        (true, 0x53, key::DELETE),
        (true, 0x52, key::INSERT),
        (false, 0x3B, key::F1),
        (false, 0x3C, key::F1 + 1),
        (false, 0x3D, key::F1 + 2),
        (false, 0x3E, key::F4),
        (false, 0x3F, key::F1 + 4),
        (false, 0x40, key::F1 + 5),
        (false, 0x41, key::F1 + 6),
        (false, 0x42, key::F1 + 7),
        (false, 0x43, key::F1 + 8),
        (false, 0x44, key::F1 + 9),
        (false, 0x57, key::F1 + 10),
        (false, 0x58, key::F12),
    ];
    let mut scancodes = Vec::new();
    for &(extended, code, _) in table {
        for release in [0u8, 0x80] {
            if extended {
                scancodes.push(0xE0);
            }
            scancodes.push(code | release);
        }
    }
    with_compositor(&scancodes, |events| {
        check!(
            events.len() == table.len() * 2,
            "got {} events, expected {}",
            events.len(),
            table.len() * 2
        );
        for (index, &(_, code, want)) in table.iter().enumerate() {
            check!(
                events[index * 2] == (3, want as i32) && events[index * 2 + 1] == (4, want as i32),
                "scancode {code:#x}: got {:?}, want down/up {want:#x}",
                &events[index * 2..index * 2 + 2]
            );
        }
        Ok(())
    })
}

/// Ctrl+letter is the letter (the compositor adds the Ctrl bit), so Ctrl+H,
/// Ctrl+I and Ctrl+M cannot be confused with Backspace, Tab and Enter; the
/// real Backspace/Tab/Enter keys keep their control values.
pub fn ctrl_letter_is_letter() -> Result<(), String> {
    let scancodes = [
        0x1D, // Ctrl down
        0x23, // h
        0x17, // i
        0x32, // m
        0x2E, // c
        0x9D, // Ctrl up
        0x0E, // Backspace
        0x0F, // Tab
        0x1C, // Enter
    ];
    with_compositor(&scancodes, |events| {
        let want: &[(u32, i32)] = &[
            (3, key::CTRL as i32),
            (3, 'h' as i32),
            (3, 'i' as i32),
            (3, 'm' as i32),
            (3, 'c' as i32),
            (4, key::CTRL as i32),
            (3, key::BACKSPACE as i32),
            (3, key::TAB as i32),
            (3, key::ENTER as i32),
        ];
        check!(events == want, "got {events:?}, want {want:?}");
        Ok(())
    })
}

/// Sustained decode: every extended and plain make code, with and without
/// Shift/Ctrl, never panics, never emits a code outside the documented
/// ranges, and a lone `0xE0` prefix never poisons the next key.
pub fn key_decode_soak() -> Result<(), String> {
    bind_compositor()?;
    for round in 0..12u32 {
        for code in 0..0x80u8 {
            for extended in [false, true] {
                if round % 3 == 0 {
                    crate::input::keyboard::push_scancode(0x36); // right Shift
                } else if round % 3 == 1 {
                    crate::input::keyboard::push_scancode(0x1D); // Ctrl
                }
                if extended {
                    crate::input::keyboard::push_scancode(0xE0);
                }
                crate::input::keyboard::push_scancode(code);
                crate::input::keyboard::push_scancode(0xE0);
                crate::input::keyboard::push_scancode(0x80 | code);
                crate::input::keyboard::push_scancode(0xB6);
                crate::input::keyboard::push_scancode(0x9D);
                for (_, value) in drain()? {
                    let value = value as u32;
                    let documented = matches!(value, 8 | 9 | 13 | 27)
                        || (0x20..=0xFF).contains(&value)
                        || (0x100..=0x10D).contains(&value)
                        || (key::F1..=key::F12).contains(&value);
                    check!(
                        documented,
                        "scancode {code:#x} produced undocumented key {value:#x}"
                    );
                }
            }
        }
    }
    unbind();
    Ok(())
}
