//! Modifier-key soaks under sustained and per-key churny holds
//! (issues #167, #175).

use super::*;

/// The modifier path under sustained load (issue #167): thousands of held
/// Alt+Tab chords must forward exactly the four expected events each
/// generation, with no state drift between press and release.
pub fn modifier_hotkey_soak() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    scratch_task()?;

    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    let mut drain = [0u8; 16];
    let seeded = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        drain.len() as u64,
    );
    check!(seeded == 1, "bind seeded {seeded} events, expected 1");

    const GENERATIONS: usize = 2000;
    let mut events = [0u8; 16 * 4];
    for generation in 0..GENERATIONS {
        // Alt down, Tab down, Tab up, Alt up: one overlay cycle.
        for scancode in [0x38u8, 0x0F, 0x8F, 0xB8] {
            crate::input::keyboard::push_scancode(scancode);
        }
        let count = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            events.as_mut_ptr() as u64,
            events.len() as u64,
        );
        check!(
            count == 4,
            "generation {generation}: polled {count} events, expected 4"
        );
        let want: &[(u32, i32)] = &[
            (3, crate::display::key::ALT as i32),
            (3, crate::display::key::TAB as i32),
            (4, crate::display::key::TAB as i32),
            (4, crate::display::key::ALT as i32),
        ];
        for (index, &expected) in want.iter().enumerate() {
            let got = event_at(&events, index);
            check!(
                got == expected,
                "generation {generation}: event {index} is {got:?}, expected {expected:?}"
            );
        }
    }

    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    crate::input::keyboard::reset();
    task::harness::reset();
    Ok(())
}

/// Two physical keys must not desync a shared modifier flag (issue #175):
/// holding both sides of Shift/Ctrl/Alt/Super and releasing one may not
/// forward a key-up (Alt+Tab must not commit early), only the last
/// release may, and a re-sent auto-repeat make code may not re-forward a
/// key-down. Each pair is exercised left-first and right-first since the
/// two sides are told apart by a different (code, extended) pattern per
/// modifier (same code/different code, non-extended/extended).
pub fn modifier_per_key_transitions() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    scratch_task()?;

    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    let mut drain = [0u8; 16];
    let seeded = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        drain.len() as u64,
    );
    check!(seeded == 1, "bind seeded {seeded} events, expected 1");

    // (name, left down, left up, right down, right up, forwarded key).
    #[allow(clippy::type_complexity)]
    let cases: &[(&str, &[u8], &[u8], &[u8], &[u8], u32)] = &[
        (
            "shift",
            &[0x2A],
            &[0xAA],
            &[0x36],
            &[0xB6],
            crate::display::key::SHIFT,
        ),
        (
            "ctrl",
            &[0x1D],
            &[0x9D],
            &[0xE0, 0x1D],
            &[0xE0, 0x9D],
            crate::display::key::CTRL,
        ),
        (
            "alt",
            &[0x38],
            &[0xB8],
            &[0xE0, 0x38],
            &[0xE0, 0xB8],
            crate::display::key::ALT,
        ),
        (
            "super",
            &[0xE0, 0x5B],
            &[0xE0, 0xDB],
            &[0xE0, 0x5C],
            &[0xE0, 0xDC],
            crate::display::key::SUPER,
        ),
    ];

    for &(name, left_down, left_up, right_down, right_up, key) in cases {
        // Left first: down forwards; the other side going down, and this
        // side going up while the other is still held, forward nothing;
        // the other side's release commits the up.
        for &sc in left_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in right_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in left_up {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in right_up {
            crate::input::keyboard::push_scancode(sc);
        }
        // Right first: the symmetric case.
        for &sc in right_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in left_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in right_up {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in left_up {
            crate::input::keyboard::push_scancode(sc);
        }
        // Auto-repeat: a re-sent make code while already held forwards
        // nothing.
        for &sc in left_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in left_down {
            crate::input::keyboard::push_scancode(sc);
        }
        for &sc in left_up {
            crate::input::keyboard::push_scancode(sc);
        }

        let mut events = [0u8; 16 * 8];
        let count = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            events.as_mut_ptr() as u64,
            events.len() as u64,
        );
        let expected: &[(u32, i32)] = &[
            (3, key as i32),
            (4, key as i32),
            (3, key as i32),
            (4, key as i32),
            (3, key as i32),
            (4, key as i32),
        ];
        check!(
            count as usize == expected.len(),
            "{name}: polled {count} events, expected {} (both-sides-held must not \
             commit an early key-up, and auto-repeat must not re-send a key-down)",
            expected.len()
        );
        for (index, &want) in expected.iter().enumerate() {
            let got = event_at(&events, index);
            check!(
                got == want,
                "{name}: event {index} is {got:?}, expected {want:?}"
            );
        }
    }

    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    crate::input::keyboard::reset();
    task::harness::reset();
    Ok(())
}

/// Per-key modifier tracking under sustained, churny left/right holds
/// (issue #175): thousands of generations of "left down, right down,
/// left up (right still held), right re-pressed (auto-repeat), right up"
/// must forward exactly one down and one up per generation, with no
/// drift in the per-key state across generations.
pub fn modifier_per_key_soak() -> Result<(), String> {
    crate::display::reset();
    crate::input::keyboard::reset();
    scratch_task()?;

    let mut info = [0u64; crate::display::INFO_WORDS];
    let code =
        process::dispatch_for_test(12, crate::display::op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    let mut drain = [0u8; 16];
    let seeded = process::dispatch_for_test(
        12,
        crate::display::op::INPUT_POLL,
        drain.as_mut_ptr() as u64,
        drain.len() as u64,
    );
    check!(seeded == 1, "bind seeded {seeded} events, expected 1");

    const GENERATIONS: usize = 3000;
    let mut events = [0u8; 16 * 2];
    for generation in 0..GENERATIONS {
        // Left Alt down, right Alt down, left Alt up (right still held),
        // right Alt down again (auto-repeat, no-op), right Alt up.
        for scancode in [0x38u8, 0xE0, 0x38, 0xB8, 0xE0, 0x38, 0xE0, 0xB8] {
            crate::input::keyboard::push_scancode(scancode);
        }
        let count = process::dispatch_for_test(
            12,
            crate::display::op::INPUT_POLL,
            events.as_mut_ptr() as u64,
            events.len() as u64,
        );
        check!(
            count == 2,
            "generation {generation}: polled {count} events, expected 2 (down, up)"
        );
        let want: &[(u32, i32)] = &[
            (3, crate::display::key::ALT as i32),
            (4, crate::display::key::ALT as i32),
        ];
        for (index, &expected) in want.iter().enumerate() {
            let got = event_at(&events, index);
            check!(
                got == expected,
                "generation {generation}: event {index} is {got:?}, expected {expected:?}"
            );
        }
    }

    let code = process::dispatch_for_test(12, crate::display::op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    crate::input::keyboard::reset();
    task::harness::reset();
    Ok(())
}
