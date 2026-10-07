//! The legacy lines on the I/O APIC (issue #616): every line routed with its
//! vector, trigger and polarity, the 8259 silenced, and masking that touches
//! only the mask bit, under a long toggle soak. On an image that keeps the
//! 8259 (`LAZYOS_IRQCHIP=pic`, or no MADT) the cases check that nothing was
//! routed and pass.

use super::fixture::*;
use super::*;
use crate::arch::ioapic::{self, Entry};
use crate::arch::irqchip::{self, Chip, VECTOR_BASE};
use ::acpi::madt::{inti, Signal};

const MASK_BIT: u64 = 1 << 16;
const LEVEL_BIT: u64 = 1 << 15;
const LOW_BIT: u64 = 1 << 13;

fn on_ioapic(test: &str) -> bool {
    let on = irqchip::chip() == Chip::IoApic;
    if !on {
        serial_println!("TEST:{test}:INFO:the 8259 delivers the lines on this image");
    }
    on
}

/// Every line but the cascade has a GSI whose entry carries its vector, the
/// MADT's trigger and polarity (PCI lines default to level, active low),
/// and this CPU as destination; the 8259 has every line masked.
pub fn ioapic_routes_every_line() -> Result<(), String> {
    let _fx = Fixture::new()?;
    if !on_ioapic("dev_ioapic_routes_every_line") {
        check!(
            (0..16).all(|line| irqchip::gsi(line).is_none()),
            "a line has a GSI without an I/O APIC"
        );
        return Ok(());
    }
    let madt = crate::arch::acpi_tables::platform()
        .and_then(|p| p.madt.as_ref().ok())
        .ok_or("I/O APIC without a MADT")?;
    let pci = crate::dev::pci_intx_lines();
    let dest = crate::arch::lapic::id() as u64;
    for line in (0..16u8).filter(|&line| line != 2) {
        let gsi = irqchip::gsi(line).ok_or(format!("line {line} has no GSI"))?;
        let (want_gsi, flags) = madt.isa_gsi(line);
        check!(
            gsi == want_gsi,
            "line {line} on GSI {gsi}, MADT says {want_gsi}"
        );
        let entry = ioapic::entry(gsi).ok_or("GSI past the chip")?;
        let bus = if pci & (1 << line) != 0 {
            Signal::PCI
        } else {
            Signal::ISA
        };
        let signal = inti(flags, bus);
        check!(
            entry & 0xFF == u64::from(VECTOR_BASE + line),
            "line {line}: vector {:#x}",
            entry & 0xFF
        );
        check!(
            (entry & LEVEL_BIT != 0) == signal.level && (entry & LOW_BIT != 0) == signal.active_low,
            "line {line}: entry {entry:#x} does not say {signal:?}"
        );
        check!(
            entry >> 56 == dest,
            "line {line}: destination {}",
            entry >> 56
        );
        check!(entry & 0x700 == 0, "line {line}: not fixed delivery");
    }
    check!(irqchip::gsi(2).is_none(), "the cascade was routed");
    for line in 1..16u8 {
        check!(
            crate::arch::pic::is_masked(line),
            "8259 line {line} is still unmasked"
        );
    }
    Ok(())
}

/// The redirection-entry encoding, bit by bit.
pub fn ioapic_entry_encoding() -> Result<(), String> {
    let entry = Entry {
        vector: 0x2B,
        signal: Signal::PCI,
        dest: 3,
        masked: true,
    };
    check!(
        entry.encode() == 0x0300_0000_0000_0000 | MASK_BIT | LEVEL_BIT | LOW_BIT | 0x2B,
        "PCI entry {:#x}",
        entry.encode()
    );
    let isa = Entry {
        vector: 0x21,
        signal: Signal::ISA,
        dest: 0,
        masked: false,
    };
    check!(isa.encode() == 0x21, "ISA entry {:#x}", isa.encode());
    Ok(())
}

/// Masking and unmasking a line flips the mask bit and nothing else, and the
/// mask reads back through `irqchip`, over 50 000 toggles.
pub fn ioapic_mask_toggle_soak() -> Result<(), String> {
    let _fx = Fixture::new()?;
    if !on_ioapic("dev_ioapic_mask_toggle_soak") {
        return Ok(());
    }
    let gsi = irqchip::gsi(LINE_C).ok_or("the test line has no GSI")?;
    let start = ioapic::entry(gsi).ok_or("no entry")?;
    check!(start & MASK_BIT != 0, "the unclaimed test line is unmasked");
    for round in 0..50_000u32 {
        let masked = round % 2 == 1;
        irqchip::set_masked(LINE_C, masked);
        let entry = ioapic::entry(gsi).ok_or("no entry")?;
        check!(
            entry & !MASK_BIT == start & !MASK_BIT,
            "round {round}: the entry changed beyond its mask: {start:#x} -> {entry:#x}"
        );
        check!(
            irqchip::is_masked(LINE_C) == masked && (entry & MASK_BIT != 0) == masked,
            "round {round}: mask did not read back"
        );
    }
    irqchip::set_masked(LINE_C, true);
    check!(
        ioapic::entry(gsi) == Some(start),
        "the entry was not restored"
    );
    // Inputs past the chip and lines past 15 are refused, never wrapped.
    check!(
        !ioapic::program(
            ioapic::pins(),
            Entry {
                vector: 0x40,
                signal: Signal::ISA,
                dest: 0,
                masked: true
            }
        ),
        "programmed past the chip"
    );
    check!(
        irqchip::is_masked(16) && irqchip::gsi(16).is_none(),
        "line 16 exists"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("dev_ioapic_routes_every_line", ioapic_routes_every_line),
    ("dev_ioapic_entry_encoding", ioapic_entry_encoding),
    ("dev_ioapic_mask_toggle_soak", ioapic_mask_toggle_soak),
];
