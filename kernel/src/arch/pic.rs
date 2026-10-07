//! 8259 PIC: remap IRQ0-15 to vectors 32-47 and send end-of-interrupt.

use super::io::outb;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Remap the PICs so IRQ0..=7 map to 32..=39 and IRQ8..=15 to 40..=47, then
/// unmask the timer (IRQ0) and keyboard (IRQ1).
///
/// # Safety
/// Must run once, before interrupts are enabled, with the PICs in their
/// power-on state; this issues the fixed ICW1-ICW4 sequence the 8259
/// datasheet requires and nothing else may write these ports concurrently.
pub unsafe fn init() {
    // ICW1: start initialisation (cascade, ICW4 needed).
    outb(PIC1_CMD, 0x11);
    outb(PIC2_CMD, 0x11);
    // ICW2: vector offsets.
    outb(PIC1_DATA, 0x20);
    outb(PIC2_DATA, 0x28);
    // ICW3: master has slave on IRQ2; slave identity.
    outb(PIC1_DATA, 0x04);
    outb(PIC2_DATA, 0x02);
    // ICW4: 8086/88 mode.
    outb(PIC1_DATA, 0x01);
    outb(PIC2_DATA, 0x01);
    // Masks: unmask IRQ0 (timer), IRQ1 (keyboard) and IRQ2 (cascade) on the
    // master; unmask IRQ12 (PS/2 mouse) on the slave.
    outb(PIC1_DATA, 0xF8); // 1111_1000
    outb(PIC2_DATA, 0xEF); // 1110_1111
}

/// Mask every line on both chips: the I/O APIC has taken over (issue #616).
/// The 8259 stays initialised (remapped above the exceptions), so a request
/// it latched can never arrive as a CPU exception, and masked it raises none.
pub fn mask_all() {
    x86_64::instructions::interrupts::without_interrupts(|| {
        // SAFETY: the IMR ports are owned by this module; interrupts are off.
        unsafe {
            outb(PIC1_DATA, 0xFF);
            outb(PIC2_DATA, 0xFF);
        }
    });
}

/// Program PIT channel 0 to fire IRQ0 at approximately `frequency` Hz.
///
/// Mode 2 (rate generator), not the square wave of mode 3: through the
/// I/O APIC's edge input QEMU raised IRQ0 twice per mode-3 period (a 200 Hz
/// tick, issue #616), while the 8259 latched one; mode 2 is one pulse per
/// period on both, and what other systems use for a periodic PIT.
///
/// # Safety
/// Must run once, before the timer IRQ is unmasked, with `frequency` in the
/// PIT's valid divisor range (dividing by zero or an out-of-range divisor
/// yields a nonsensical rate, not a memory-safety issue, but is still a
/// protocol violation the caller must avoid).
pub unsafe fn init_pit(frequency: u32) {
    let divisor = (1_193_182 / frequency) as u16;
    outb(0x43, 0x34); // channel 0, lobyte/hibyte, mode 2 (rate generator)
    outb(0x40, (divisor & 0xFF) as u8);
    outb(0x40, (divisor >> 8) as u8);
}

/// Data port of the PIC that owns `line` and the bit within its mask register.
const fn mask_port(line: u8) -> (u16, u8) {
    if line < 8 {
        (PIC1_DATA, 1 << line)
    } else {
        (PIC2_DATA, 1 << (line - 8))
    }
}

/// Mask (`true`) or unmask (`false`) one line (0-15) in the interrupt mask
/// register. The read-modify-write runs with interrupts off: the device IRQ
/// handler masks lines from interrupt context, and an interleaved handler would
/// otherwise have its mask overwritten by our stale copy.
///
/// Unmasking a slave line (8-15) relies on the cascade (IRQ2), which [`init`]
/// leaves unmasked.
///
/// Line 0 means "the scheduler tick": when the local APIC timer is the tick
/// source (`arch::timer`), it masks that timer and the PIT's line stays
/// masked.
pub fn set_masked(line: u8, masked: bool) {
    if line >= 16 {
        return;
    }
    if line == 0 && super::timer::lapic_tick() {
        super::timer::set_tick_masked(masked);
        return;
    }
    let (port, bit) = mask_port(line);
    if line == 0 && !masked {
        // Time with the timer masked is not "lost" to a long syscall: start
        // counting periods from now (issue #344).
        super::clock::resync();
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        // SAFETY: the IMR ports are owned by this module and the RMW is atomic
        // with respect to interrupts on this single CPU.
        unsafe {
            let current = super::io::inb(port);
            let next = if masked {
                current | bit
            } else {
                current & !bit
            };
            super::io::outb(port, next);
        }
    });
}

/// Whether `line` is currently masked at the PIC. Lines outside 0-15 read as
/// masked (they cannot be delivered).
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // the suite reads mask state
pub fn is_masked(line: u8) -> bool {
    if line >= 16 {
        return true;
    }
    if line == 0 && super::timer::lapic_tick() {
        return super::timer::tick_masked();
    }
    let (port, bit) = mask_port(line);
    // SAFETY: reading the IMR has no side effects.
    unsafe { super::io::inb(port) & bit != 0 }
}

/// Send a *specific* end-of-interrupt for `line`. Unlike the non-specific EOI
/// this can never retire a different in-service interrupt, so it is safe for
/// the generic device handlers (and harmless if `line` is not in service).
///
/// # Safety
/// Call from the handler of `line`, once per interrupt.
pub unsafe fn end_of_interrupt_specific(line: u8) {
    if line >= 8 {
        super::io::outb(PIC2_CMD, 0x60 | (line - 8));
        // The slave is chained through master IRQ2.
        super::io::outb(PIC1_CMD, 0x60 | 2);
    } else {
        super::io::outb(PIC1_CMD, 0x60 | line);
    }
}

/// Whether `line` is genuinely in service according to the PIC's in-service
/// register. Used to tell a real IRQ 7 / IRQ 15 from a spurious one (the PIC
/// raises them when an interrupt is withdrawn between INTR and the acknowledge
/// cycle).
///
/// # Safety
/// Must run from the handler of `line`: OCW3 selects the ISR for the next read.
pub unsafe fn in_service(line: u8) -> bool {
    let (cmd, bit) = if line >= 8 {
        (PIC2_CMD, 1 << (line - 8))
    } else {
        (PIC1_CMD, 1 << line)
    };
    super::io::outb(cmd, 0x0B); // OCW3: next read returns the ISR
    let in_service = super::io::inb(cmd) & bit != 0;
    super::io::outb(cmd, 0x0A); // back to the default: reads return the IRR
    in_service
}

/// Whether a device is asserting `line` into the PIC right now, according to
/// the interrupt request register. Works while the line is masked, which is
/// how the suite proves a PCI function is wired to the PIC line its
/// Interrupt Line register names without taking the interrupt.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // only the suite probes the IRR
pub fn requested(line: u8) -> bool {
    if line >= 16 {
        return false;
    }
    let (cmd, bit) = if line >= 8 {
        (PIC2_CMD, 1 << (line - 8))
    } else {
        (PIC1_CMD, 1 << line)
    };
    x86_64::instructions::interrupts::without_interrupts(|| {
        // SAFETY: OCW3 read-IRR commands and the following read touch only the
        // PIC's command port and change no interrupt state; interrupts are off
        // so a handler cannot re-select the ISR between the two accesses.
        unsafe {
            super::io::outb(cmd, 0x0A);
            super::io::inb(cmd) & bit != 0
        }
    })
}
