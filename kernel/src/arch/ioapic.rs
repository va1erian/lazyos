//! The I/O APIC (issue #616): the chip that replaces the 8259 for the legacy
//! lines once `irqchip` switches to it.
//!
//! Only the I/O APIC whose inputs start at GSI 0 is driven: it carries the
//! sixteen ISA inputs (directly or through the MADT's interrupt source
//! overrides), which is every line LazyOS routes. Its registers sit behind an
//! index/data pair (`IOREGSEL`, `IOWIN`), so each access is a select followed
//! by a read or write; the pair runs with interrupts off because the device
//! interrupt handler masks lines from interrupt context, and an interrupted
//! select would send the task's write to the wrong register.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use ::acpi::madt::{IoApic, Signal};

use crate::mem;

const IOREGSEL: u64 = 0x00;
const IOWIN: u64 = 0x10;
const REG_VERSION: u32 = 0x01;
/// First redirection-table register; entry `n` is `0x10 + 2n` (low) and
/// `0x11 + 2n` (high).
const REG_REDIRECTION: u32 = 0x10;

const ENTRY_POLARITY_LOW: u64 = 1 << 13;
const ENTRY_REMOTE_IRR: u64 = 1 << 14;
const ENTRY_LEVEL: u64 = 1 << 15;
const ENTRY_MASKED: u64 = 1 << 16;
const ENTRY_DEST_SHIFT: u32 = 56;

/// Kernel virtual address of the register window (0: no I/O APIC).
static WINDOW: AtomicU64 = AtomicU64::new(0);
/// Inputs the chip has (from its version register).
static PINS: AtomicU32 = AtomicU32::new(0);

/// How one input is delivered: always fixed delivery, physical destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub vector: u8,
    pub signal: Signal,
    /// The destination local APIC's ID (xAPIC IDs fit in eight bits).
    pub dest: u8,
    pub masked: bool,
}

impl Entry {
    /// The 64-bit redirection-table form.
    pub fn encode(self) -> u64 {
        let mut word = u64::from(self.vector) | (u64::from(self.dest) << ENTRY_DEST_SHIFT);
        if self.signal.active_low {
            word |= ENTRY_POLARITY_LOW;
        }
        if self.signal.level {
            word |= ENTRY_LEVEL;
        }
        if self.masked {
            word |= ENTRY_MASKED;
        }
        word
    }
}

fn read(reg: u32) -> u32 {
    let base = WINDOW.load(Ordering::Relaxed);
    x86_64::instructions::interrupts::without_interrupts(|| {
        // SAFETY: `init` mapped the 4 KiB register page uncached and stored
        // its address; `IOREGSEL` and `IOWIN` are aligned 32-bit registers
        // inside it, and interrupts are off so the select is not raced.
        unsafe {
            ((base + IOREGSEL) as *mut u32).write_volatile(reg);
            ((base + IOWIN) as *const u32).read_volatile()
        }
    })
}

fn write(reg: u32, value: u32) {
    let base = WINDOW.load(Ordering::Relaxed);
    x86_64::instructions::interrupts::without_interrupts(|| {
        // SAFETY: as in `read`.
        unsafe {
            ((base + IOREGSEL) as *mut u32).write_volatile(reg);
            ((base + IOWIN) as *mut u32).write_volatile(value);
        }
    });
}

/// Map `chip`'s registers, read how many inputs it has and mask them all.
/// Returns the input count.
pub fn init(chip: IoApic) -> Result<u32, &'static str> {
    if chip.gsi_base != 0 {
        return Err("no I/O APIC starts at GSI 0");
    }
    let phys = u64::from(chip.address);
    if phys == 0 || phys & 0xF != 0 {
        return Err("I/O APIC address is not aligned");
    }
    let page = mem::mmio::map_kernel(phys & !0xFFF, 4096)?;
    WINDOW.store(page + (phys & 0xFFF), Ordering::Relaxed);
    let version = read(REG_VERSION);
    // Bits 16..24 hold the index of the last entry; all-ones is no chip.
    let pins = ((version >> 16) & 0xFF) + 1;
    if version == u32::MAX || !(16..=240).contains(&pins) {
        WINDOW.store(0, Ordering::Relaxed);
        return Err("I/O APIC version register is not plausible");
    }
    PINS.store(pins, Ordering::Relaxed);
    for gsi in 0..pins {
        write_entry(gsi, ENTRY_MASKED);
    }
    Ok(pins)
}

/// Inputs on the driven chip (0 before [`init`]).
pub fn pins() -> u32 {
    PINS.load(Ordering::Relaxed)
}

fn write_entry(gsi: u32, word: u64) {
    // Mask first, then the high half, then the low half with the final mask
    // bit, so the chip never sees a half-written unmasked entry.
    let reg = REG_REDIRECTION + gsi * 2;
    write(reg, read(reg) | ENTRY_MASKED as u32);
    write(reg + 1, (word >> 32) as u32);
    write(reg, word as u32);
}

/// The raw redirection entry for `gsi`, if the chip has that input.
pub fn entry(gsi: u32) -> Option<u64> {
    if gsi >= pins() {
        return None;
    }
    let reg = REG_REDIRECTION + gsi * 2;
    Some(u64::from(read(reg)) | (u64::from(read(reg + 1)) << 32))
}

/// Program input `gsi`. Inputs past the chip are ignored (returns false).
pub fn program(gsi: u32, entry: Entry) -> bool {
    if gsi >= pins() {
        return false;
    }
    write_entry(gsi, entry.encode());
    true
}

/// Mask or unmask input `gsi`, leaving the rest of its entry as programmed.
pub fn set_masked(gsi: u32, masked: bool) {
    if gsi >= pins() {
        return;
    }
    let reg = REG_REDIRECTION + gsi * 2;
    x86_64::instructions::interrupts::without_interrupts(|| {
        let low = read(reg);
        let next = if masked {
            low | ENTRY_MASKED as u32
        } else {
            low & !(ENTRY_MASKED as u32)
        };
        if next != low {
            write(reg, next);
        }
    });
}

/// Whether input `gsi` is masked (inputs past the chip read as masked).
pub fn is_masked(gsi: u32) -> bool {
    entry(gsi).is_none_or(|word| word & ENTRY_MASKED != 0)
}

/// Whether a level-triggered input was delivered and awaits its EOI.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn remote_irr(gsi: u32) -> bool {
    entry(gsi).is_some_and(|word| word & ENTRY_REMOTE_IRR != 0)
}
