//! The legacy interrupt lines 0-15 on whichever controller delivers them
//! (issue #616).
//!
//! Boot starts on the 8259 (`pic`): the PIT probe reads its request register
//! and nothing else is known yet. Once the tick is chosen, [`init`] moves
//! every line to the I/O APIC when the MADT names one and the local APIC is
//! usable, and the 8259 is masked for good (the local APIC's LINT0 too, so
//! not even a spurious IRQ 7 gets through). Each ISA line keeps its vector
//! (32 + line), so every handler stays where it was; what changes is how a
//! line is masked and acknowledged, which is all this module answers.
//!
//! An ISA line goes to the GSI and with the trigger and polarity the MADT's
//! interrupt source overrides give it. A line a PCI function names in its
//! Interrupt Line register defaults to level, active low (PCI INTx) when no
//! override describes it; QEMU's machines override those lines to level,
//! active high. That register is the firmware's 8259 routing: on QEMU the
//! same wire reaches the I/O APIC input of that number, but a real chipset in
//! APIC mode wires PCI interrupts to inputs 16-23 through the DSDT's `_PRT`,
//! which needs an AML interpreter. Real hardware is expected to use MSI
//! (`dev::msi`); an INTx device there may stay silent and poll.
//!
//! `LAZYOS_IRQCHIP=pic` keeps the 8259 (and virtual wire); the default picks
//! the I/O APIC when it can. One line reports the outcome:
//! `HW:IRQCHIP:<pic|ioapic> ...`.

use core::sync::atomic::{AtomicU32, AtomicU8, Ordering};

use ::acpi::madt::{inti, Signal};

use super::ioapic::{self, Entry};
use super::{acpi_tables, clock, lapic, pic, timer};

/// Legacy lines.
pub const LINES: u8 = 16;
/// Vector of line 0; line `n` is `VECTOR_BASE + n` on both controllers.
pub const VECTOR_BASE: u8 = 32;
/// [`GSI`] value for a line the I/O APIC cannot deliver.
const NO_GSI: u32 = u32::MAX;

/// 0: the 8259, 1: the I/O APIC.
static CHIP: AtomicU8 = AtomicU8::new(0);
static GSI: [AtomicU32; LINES as usize] = [const { AtomicU32::new(NO_GSI) }; LINES as usize];

/// The controller that delivers the legacy lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    Pic,
    IoApic,
}

pub fn chip() -> Chip {
    if CHIP.load(Ordering::Relaxed) == 1 {
        Chip::IoApic
    } else {
        Chip::Pic
    }
}

/// The GSI line `line` was routed to (I/O APIC only).
pub fn gsi(line: u8) -> Option<u32> {
    let gsi = GSI.get(usize::from(line))?.load(Ordering::Relaxed);
    (gsi != NO_GSI).then_some(gsi)
}

/// Pick the controller. Runs once from `idt::init_hardware`, after the tick
/// is chosen and the PCI functions are enumerated, with interrupts off.
pub fn init() {
    if option_env!("LAZYOS_IRQCHIP") == Some("pic") {
        crate::serial_println!("HW:IRQCHIP:pic forced (LAZYOS_IRQCHIP=pic)");
        return;
    }
    match switch_to_ioapic() {
        Ok((pins, dest)) => {
            crate::serial_println!(
                "HW:IRQCHIP:ioapic pins={pins} dest={dest} pci_lines={:#06x}",
                crate::dev::pci_intx_lines()
            );
            for line in 0..LINES {
                if let Some(gsi) = gsi(line) {
                    let entry = ioapic::entry(gsi).unwrap_or(0);
                    crate::serial_println!("irqchip: line {line} -> gsi {gsi} entry {entry:#x}");
                }
            }
        }
        Err(why) => crate::serial_println!("HW:IRQCHIP:pic ({why})"),
    }
}

/// Route every line through the I/O APIC, carrying over each line's mask,
/// then silence the 8259. Returns the chip's input count and the APIC ID
/// interrupts are sent to.
fn switch_to_ioapic() -> Result<(u32, u8), &'static str> {
    let platform = acpi_tables::platform().ok_or("no ACPI tables")?;
    let madt = platform.madt.as_ref().map_err(|_| "no usable MADT")?;
    let chip = madt
        .ioapics
        .as_slice()
        .iter()
        .find(|io| io.gsi_base == 0)
        .copied()
        .ok_or("no I/O APIC at GSI 0")?;
    lapic::init(Some(madt.lapic_address))?;
    let dest = u8::try_from(lapic::id()).map_err(|_| "APIC ID above 255")?;
    let pins = ioapic::init(chip)?;
    // The cascade carries no device of its own, and its GSI is the PIT's on
    // every PC that overrides IRQ 0 to GSI 2.
    let routed = (0..LINES).filter(|&line| line != 2);
    // Every line must reach this chip before the 8259 is given up: a line
    // left behind would be dead on both. `ioapic::init` left every input
    // masked, so refusing here leaves the 8259 in charge as it was.
    if routed.clone().any(|line| madt.isa_gsi(line).0 >= pins) {
        return Err("a legacy line's GSI is past the I/O APIC at GSI 0");
    }
    let pci = crate::dev::pci_intx_lines();
    for line in routed {
        let (gsi, flags) = madt.isa_gsi(line);
        let bus = if pci & (1 << line) != 0 {
            Signal::PCI
        } else {
            Signal::ISA
        };
        // Whatever the 8259 had unmasked stays unmasked; with the APIC timer
        // as the tick, the PIT's line stays off.
        let masked = if line == 0 && timer::lapic_tick() {
            true
        } else {
            pic::is_masked(line)
        };
        let entry = Entry {
            vector: VECTOR_BASE + line,
            signal: inti(flags, bus),
            dest,
            masked,
        };
        if ioapic::program(gsi, entry) {
            GSI[usize::from(line)].store(gsi, Ordering::Relaxed);
        }
    }
    pic::mask_all();
    lapic::mask_lint0();
    CHIP.store(1, Ordering::Release);
    Ok((pins, dest))
}

/// Mask (`true`) or unmask (`false`) line `line`. Line 0 means "the tick":
/// with the APIC timer as the tick it masks that timer instead.
pub fn set_masked(line: u8, masked: bool) {
    if line >= LINES {
        return;
    }
    if chip() == Chip::Pic || (line == 0 && timer::lapic_tick()) {
        pic::set_masked(line, masked);
        return;
    }
    let Some(gsi) = gsi(line) else { return };
    if line == 0 && !masked {
        // As on the 8259: time spent masked is not lost (issue #344).
        clock::resync();
    }
    ioapic::set_masked(gsi, masked);
}

/// Whether a request that arrives on `line` while it is masked is still
/// delivered once it is unmasked: always on the 8259 (its request register
/// latches the edge), and on the I/O APIC only for a level-triggered input
/// (the device keeps asserting; an edge that arrives masked is lost). The
/// interrupt rate limit (`dev::throttle`) holds only such lines.
pub fn masked_keeps_request(line: u8) -> bool {
    if line >= LINES {
        return false;
    }
    if chip() == Chip::Pic {
        return true;
    }
    /// Redirection entry bit 15: level-triggered.
    const LEVEL: u64 = 1 << 15;
    gsi(line)
        .and_then(ioapic::entry)
        .is_some_and(|entry| entry & LEVEL != 0)
}

/// Whether `line` is masked (lines that cannot be delivered read masked).
pub fn is_masked(line: u8) -> bool {
    if line >= LINES {
        return true;
    }
    if chip() == Chip::Pic || (line == 0 && timer::lapic_tick()) {
        return pic::is_masked(line);
    }
    gsi(line).is_none_or(ioapic::is_masked)
}

/// Acknowledge the interrupt on `line` at the controller that raised it.
///
/// # Safety
/// Call from the handler of `line`, once per interrupt.
pub unsafe fn eoi(line: u8) {
    match chip() {
        Chip::Pic => pic::end_of_interrupt_specific(line),
        Chip::IoApic => lapic::eoi(),
    }
}

/// Whether an interrupt that arrived on `line` is spurious: the 8259 raises
/// IRQ 7 or 15 when a request vanishes during its acknowledge cycle. The
/// I/O APIC has no such lines (its spurious interrupts use the local APIC's
/// spurious vector), and with it in charge the 8259 is masked off entirely.
///
/// # Safety
/// Call from the handler of `line` (the 8259's in-service read needs it).
pub unsafe fn spurious(line: u8) -> bool {
    chip() == Chip::Pic && (line == 7 || line == 15) && !pic::in_service(line)
}

/// Whether a device is asserting `line` right now. The 8259 shows that in
/// its request register even while the line is masked; the I/O APIC only
/// once it delivered the vector (unmasked, interrupts off): the request
/// then waits in the local APIC, or, level-triggered, holds remote IRR.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn requested(line: u8) -> bool {
    match chip() {
        Chip::Pic => pic::requested(line),
        Chip::IoApic => {
            line < LINES
                && (lapic::requested(VECTOR_BASE + line)
                    || gsi(line).is_some_and(ioapic::remote_irr))
        }
    }
}
