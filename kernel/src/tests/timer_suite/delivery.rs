//! Interrupt delivery around the tick: masking line 0 stops whichever source
//! is the tick (and the APIC EOI keeps it coming), PIT-dead detection keeps
//! its verdict under load, and the i8042's IRQ1 and IRQ12 still arrive
//! through the 8259 (virtual wire, LINT0 ExtINT, when the APIC is on).

use super::*;
use crate::arch::io::{inb, outb};
use crate::arch::refclock::{RefClock, Stopwatch};
use crate::input::{keyboard, mouse};

/// Spin bound for waits that end on an interrupt.
const SPINS: u32 = 50_000_000;

/// Spin until `TICKS` moves from `from`; false if no tick came.
fn wait_tick(from: u64) -> bool {
    (0..SPINS).any(|_| {
        core::hint::spin_loop();
        task::ticks() != from
    })
}

/// Busy-wait about `ms` milliseconds on the reference clock (or the TSC
/// rate the clock calibrated, or a plain spin without either).
fn busy_ms(reference: Option<RefClock>, ms: u64) {
    if let Some(clock) = reference {
        let mut watch = Stopwatch::start(clock);
        for _ in 0..SPINS {
            if watch.elapsed_ns() >= ms * 1_000_000 {
                return;
            }
        }
        return;
    }
    let per_ms = crate::arch::clock::cycles_per_tick() / 10;
    // SAFETY: `rdtsc` only reads the time-stamp counter.
    let start = unsafe { core::arch::x86_64::_rdtsc() };
    for _ in 0..SPINS {
        // SAFETY: as above.
        if per_ms != 0 && unsafe { core::arch::x86_64::_rdtsc() } - start >= ms * per_ms {
            return;
        }
        core::hint::spin_loop();
    }
}

/// The boot verdict holds on every re-probe: 24 rounds alternating between
/// interrupts off and the tick firing during the probe, with allocation
/// churn between rounds. A live PIT is never called frozen (and a frozen
/// one, as under `-machine pit=off`, never called alive).
pub fn pit_probe_no_false_trigger() -> Result<(), String> {
    let info = info()?;
    if info.pit == PitVerdict::Unknown {
        serial_println!("TEST:timer_pit_probe_no_false_trigger:INFO:no reference clock; skipped");
        return Ok(());
    }
    let expected = match info.pit {
        PitVerdict::Frozen => PitVerdict::Frozen,
        _ => PitVerdict::Alive,
    };
    kernel_only();
    for round in 0..24u32 {
        let churn: Vec<Vec<u8>> = (0..32).map(|i| vec![(round + i) as u8; 2048]).collect();
        let probe = if round % 2 == 0 {
            timer::probe_pit(info.reference, false)
        } else {
            with_lines(&[0], || timer::probe_pit(info.reference, false))
        };
        drop(churn);
        check!(
            probe.verdict == expected,
            "round {round}: {:?} ({} changes in {} reads over {:?} ns), boot said {:?}",
            probe.verdict,
            probe.changes,
            probe.reads,
            probe.window_ns,
            info.pit
        );
    }
    Ok(())
}

/// Masking line 0 stops the tick, whatever drives it; unmasking restarts it
/// without catching up the masked time.
pub fn mask_stops_the_tick() -> Result<(), String> {
    let reference = info()?.reference;
    kernel_only();
    with_lines(&[0], || {
        check!(wait_tick(task::ticks()), "no tick with line 0 open");
        irqchip::set_masked(0, true);
        check!(irqchip::is_masked(0), "line 0 does not read back masked");
        let before = task::ticks();
        busy_ms(reference, 100);
        let after = task::ticks();
        check!(after == before, "{} ticks while masked", after - before);
        // SAFETY: `rdtsc` only reads the time-stamp counter.
        let unmasked_at = unsafe { core::arch::x86_64::_rdtsc() };
        irqchip::set_masked(0, false);
        check!(!irqchip::is_masked(0), "line 0 does not read back open");
        check!(wait_tick(after), "no tick after unmasking");
        let moved = task::ticks() - after;
        // Only the time since the unmask may be counted (TCG can deliver the
        // first tick late), never the ten masked periods.
        // SAFETY: as above.
        let since = unsafe { core::arch::x86_64::_rdtsc() } - unmasked_at;
        let periods = since / crate::arch::clock::cycles_per_tick().max(1);
        check!(
            moved <= periods + 2 && moved < 10,
            "unmasking counted {moved} ticks, {periods} periods after the unmask"
        );
        Ok(())
    })
}

/// Soak: 400 mask/unmask cycles with interrupts on, then the tick still
/// runs at its rate (a missing APIC EOI would stop it after one tick).
pub fn mask_unmask_soak() -> Result<(), String> {
    let reference = info()?.reference;
    kernel_only();
    with_lines(&[0], || {
        for round in 0..400u32 {
            irqchip::set_masked(0, true);
            if round % 50 == 0 {
                busy_ms(reference, 1);
            }
            irqchip::set_masked(0, false);
        }
        let start = task::ticks();
        busy_ms(reference, 200);
        let moved = task::ticks() - start;
        check!(
            (10..=40).contains(&moved),
            "{moved} ticks in 200 ms after the soak"
        );
        Ok(())
    })
}

/// Wait until the i8042 accepts a byte; false if it never does.
fn i8042_ready() -> bool {
    // SAFETY: reading the i8042 status register has no side effects.
    (0..1_000_000).any(|_| unsafe { inb(0x64) } & 0x02 == 0)
}

/// Make the i8042 report `bytes` as if the keyboard (`0xD2`) or the mouse
/// (`0xD3`) had sent them: each raises IRQ1 or IRQ12.
fn i8042_inject(command: u8, bytes: &[u8]) -> Result<(), String> {
    for &byte in bytes {
        check!(i8042_ready(), "the i8042 input buffer never drained");
        // SAFETY: 0xD2/0xD3 are the i8042's "write output buffer" commands;
        // the controller places the next data byte in its output buffer.
        unsafe { outb(0x64, command) };
        check!(i8042_ready(), "the i8042 did not take the command");
        // SAFETY: the data byte for the command above.
        unsafe { outb(0x60, byte) };
        // The handler must drain each byte before the next is written.
        let drained = (0..SPINS).any(|_| {
            core::hint::spin_loop();
            // SAFETY: reading the i8042 status register has no side effects.
            (unsafe { inb(0x64) } & 0x01) == 0
        });
        check!(
            drained,
            "IRQ {} never drained byte {byte:#x}",
            if command == 0xD2 { 1 } else { 12 }
        );
    }
    Ok(())
}

/// A scancode injected through the i8042 reaches the IRQ1 handler.
pub fn keyboard_irq_through_pic() -> Result<(), String> {
    keyboard::reset();
    let repeats = crate::input::raw_tap::suppressed_repeats();
    let result = with_lines(&[1], || {
        // 0x1E twice: the A key pressed, then a typematic repeat the raw tap
        // counts, which only the IRQ1 handler can have fed it; then released.
        i8042_inject(0xD2, &[0x1E, 0x1E, 0x9E])?;
        let seen = crate::input::raw_tap::suppressed_repeats() - repeats;
        check!(seen == 1, "IRQ1 fed the raw tap {seen} repeats, expected 1");
        Ok(())
    });
    keyboard::reset();
    result
}

/// A mouse packet injected through the i8042 reaches the IRQ12 handler
/// (slave PIC, through the cascade).
pub fn mouse_irq_through_cascade() -> Result<(), String> {
    mouse::set_wheel_mode_for_test(false);
    let _ = mouse::take_moved();
    let result = with_lines(&[12], || {
        // Buttons none (bit 3 always set), dx = 5, dy = 0.
        i8042_inject(0xD3, &[0x08, 0x05, 0x00])?;
        check!(mouse::take_moved().is_some(), "IRQ12 delivered no movement");
        Ok(())
    });
    mouse::set_wheel_mode_for_test(false);
    result
}
