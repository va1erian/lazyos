//! 8259 PIC: remap IRQ0-15 to vectors 32-47 and send end-of-interrupt.

use x86_64::instructions::port::Port;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// Remap the PICs so IRQ0..=7 map to 32..=39 and IRQ8..=15 to 40..=47, then
/// unmask the timer (IRQ0) and keyboard (IRQ1).
pub unsafe fn init() {
    // ICW1: start initialisation (cascade, ICW4 needed).
    Port::<u8>::new(PIC1_CMD).write(0x11);
    Port::<u8>::new(PIC2_CMD).write(0x11);
    // ICW2: vector offsets.
    Port::<u8>::new(PIC1_DATA).write(0x20);
    Port::<u8>::new(PIC2_DATA).write(0x28);
    // ICW3: master has slave on IRQ2; slave identity.
    Port::<u8>::new(PIC1_DATA).write(0x04);
    Port::<u8>::new(PIC2_DATA).write(0x02);
    // ICW4: 8086/88 mode.
    Port::<u8>::new(PIC1_DATA).write(0x01);
    Port::<u8>::new(PIC2_DATA).write(0x01);
    // Masks: unmask IRQ0 + IRQ1 on the master, mask everything on the slave.
    Port::<u8>::new(PIC1_DATA).write(0xFC);
    Port::<u8>::new(PIC2_DATA).write(0xFF);
}

/// Signal end-of-interrupt for `irq` (0-15).
pub unsafe fn end_of_interrupt(irq: u8) {
    if irq >= 8 {
        Port::<u8>::new(PIC2_CMD).write(0x20);
    }
    Port::<u8>::new(PIC1_CMD).write(0x20);
}

/// Program PIT channel 0 to fire IRQ0 at approximately `frequency` Hz.
pub unsafe fn init_pit(frequency: u32) {
    let divisor = (1_193_182 / frequency) as u16;
    let mut command = Port::<u8>::new(0x43);
    let mut channel0 = Port::<u8>::new(0x40);
    command.write(0x36); // channel 0, lobyte/hibyte, mode 3 (square wave)
    channel0.write((divisor & 0xFF) as u8);
    channel0.write((divisor >> 8) as u8);
}
