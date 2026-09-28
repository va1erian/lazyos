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

/// Signal end-of-interrupt for `irq` (0-15).
///
/// # Safety
/// Must be called from the corresponding IRQ handler, after servicing it,
/// exactly once per interrupt; sending EOI out of order or spuriously can
/// desynchronise the PIC's in-service state.
pub unsafe fn end_of_interrupt(irq: u8) {
    if irq >= 8 {
        outb(PIC2_CMD, 0x20);
    }
    outb(PIC1_CMD, 0x20);
}

/// Program PIT channel 0 to fire IRQ0 at approximately `frequency` Hz.
///
/// # Safety
/// Must run once, before the timer IRQ is unmasked, with `frequency` in the
/// PIT's valid divisor range (dividing by zero or an out-of-range divisor
/// yields a nonsensical rate, not a memory-safety issue, but is still a
/// protocol violation the caller must avoid).
pub unsafe fn init_pit(frequency: u32) {
    let divisor = (1_193_182 / frequency) as u16;
    outb(0x43, 0x36); // channel 0, lobyte/hibyte, mode 3 (square wave)
    outb(0x40, (divisor & 0xFF) as u8);
    outb(0x40, (divisor >> 8) as u8);
}
