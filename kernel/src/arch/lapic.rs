//! The bootstrap processor's local APIC: enabled in virtual-wire mode so the
//! 8259 keeps delivering device interrupts, and its timer used as the
//! scheduler tick when the PIT does not tick (docs/real-pc-boot-plan.md H2).
//!
//! Virtual wire: LINT0 is programmed `ExtINT`, so the 8259's INTR passes
//! through the local APIC unchanged and the CPU fetches the vector from the
//! PIC as before. ExtINT deliveries are not tracked in the APIC's in-service
//! register, so the PIC handlers keep sending only the 8259 EOI; only the
//! APIC timer vector takes an APIC EOI. LINT1 is NMI, as firmware leaves it.
//!
//! Register access is x2APIC (MSRs `0x800 + offset / 16`) when firmware left
//! x2APIC mode on (modern UEFI machines may; xAPIC MMIO is then dead), else
//! xAPIC MMIO through the physical-memory map with the page made uncached.
//! `LAZYOS_X2APIC=1` makes the kernel switch to x2APIC itself when the CPU
//! has it, to exercise that path under QEMU.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

use x86_64::PhysAddr;

use super::msr;
use crate::mem;

/// Vector of the APIC timer as the tick: above the remapped PIC (32..48),
/// below the syscall (0x80) and reschedule (0x81) gates. As the deadline
/// timer it uses the next vector (`event_timer::VECTOR`).
pub const TIMER_VECTOR: u8 = 0x30;
/// Spurious-interrupt vector; the low four bits must be set on old CPUs.
pub const SPURIOUS_VECTOR: u8 = 0xFF;

const IA32_APIC_BASE: u32 = 0x1B;
const BASE_ENABLE: u64 = 1 << 11;
const BASE_X2APIC: u64 = 1 << 10;
const BASE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;

// Register offsets (xAPIC MMIO layout).
const ID: u32 = 0x20;
const TPR: u32 = 0x80;
const EOI: u32 = 0xB0;
const SVR: u32 = 0xF0;
const ESR: u32 = 0x280;
const LVT_TIMER: u32 = 0x320;
const LVT_LINT0: u32 = 0x350;
const LVT_LINT1: u32 = 0x360;
const LVT_ERROR: u32 = 0x370;
const TIMER_INITIAL: u32 = 0x380;
const TIMER_CURRENT: u32 = 0x390;
const TIMER_DIVIDE: u32 = 0x3E0;

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const DELIVERY_NMI: u32 = 0b100 << 8;
const DELIVERY_EXTINT: u32 = 0b111 << 8;
/// Divide configuration for "divide by 16" (bits 0, 1 and 3 encode it).
const DIVIDE_BY_16: u32 = 0b0011;
/// The timer counts the APIC timer clock divided by this.
pub const DIVISOR: u64 = 16;

/// How the registers are reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    XApic,
    X2Apic,
}

/// 0: not enabled, 1: xAPIC, 2: x2APIC.
static MODE: AtomicU8 = AtomicU8::new(0);
/// Virtual address of the xAPIC register page.
static MMIO: AtomicU64 = AtomicU64::new(0);

fn mode() -> Option<Mode> {
    match MODE.load(Ordering::Relaxed) {
        1 => Some(Mode::XApic),
        2 => Some(Mode::X2Apic),
        _ => None,
    }
}

fn read(reg: u32) -> u32 {
    match mode() {
        Some(Mode::X2Apic) => msr::read(0x800 + (reg >> 4)) as u32,
        Some(Mode::XApic) => {
            let at = (MMIO.load(Ordering::Relaxed) + u64::from(reg)) as *const u32;
            // SAFETY: `init` mapped the register page (uncached) and recorded
            // its address; `reg` is one of this module's aligned offsets.
            unsafe { at.read_volatile() }
        }
        None => 0,
    }
}

fn write(reg: u32, value: u32) {
    match mode() {
        Some(Mode::X2Apic) => msr::write(0x800 + (reg >> 4), u64::from(value)),
        Some(Mode::XApic) => {
            let at = (MMIO.load(Ordering::Relaxed) + u64::from(reg)) as *mut u32;
            // SAFETY: as in `read`; a 32-bit aligned register store.
            unsafe { at.write_volatile(value) }
        }
        None => {}
    }
}

/// `CPUID` leaf 1: (ECX, EDX).
fn cpuid1() -> (u32, u32) {
    let r = core::arch::x86_64::__cpuid(1);
    (r.ecx, r.edx)
}

/// Bring the local APIC up in virtual-wire mode with its timer masked.
/// `madt_address` is only cross-checked: `IA32_APIC_BASE` is authoritative.
pub fn init(madt_address: Option<u64>) -> Result<Mode, &'static str> {
    let (ecx, edx) = cpuid1();
    if edx & (1 << 9) == 0 {
        return Err("no local APIC");
    }
    let mut base = msr::read(IA32_APIC_BASE);
    if base & BASE_ENABLE == 0 {
        base |= BASE_ENABLE;
        msr::write(IA32_APIC_BASE, base);
    }
    let want_x2 = cfg!(lazyos_x2apic) && ecx & (1 << 21) != 0;
    if base & BASE_X2APIC == 0 && want_x2 {
        base |= BASE_X2APIC;
        msr::write(IA32_APIC_BASE, base);
    }
    let mode = if base & BASE_X2APIC != 0 {
        MODE.store(2, Ordering::Relaxed);
        Mode::X2Apic
    } else {
        map_xapic(base & BASE_ADDR)?;
        MODE.store(1, Ordering::Relaxed);
        Mode::XApic
    };
    if let Some(addr) = madt_address.filter(|&a| a != base & BASE_ADDR) {
        crate::serial_println!(
            "lapic: MADT says {addr:#x}, IA32_APIC_BASE says {:#x}; using the MSR",
            base & BASE_ADDR
        );
    }
    // Software-enable first: while disabled, LVT mask bits cannot be cleared.
    write(SVR, SVR_ENABLE | u32::from(SPURIOUS_VECTOR));
    write(TPR, 0);
    write(LVT_TIMER, LVT_MASKED | u32::from(TIMER_VECTOR));
    write(LVT_ERROR, LVT_MASKED | u32::from(SPURIOUS_VECTOR));
    write(LVT_LINT0, DELIVERY_EXTINT);
    write(LVT_LINT1, DELIVERY_NMI);
    // Clear any error latched before we took over (write, then read).
    write(ESR, 0);
    let _ = read(ESR);
    crate::serial_println!(
        "lapic: {:?} id={:#x} virtual wire (LINT0 ExtINT)",
        mode,
        id()
    );
    Ok(mode)
}

/// Map the xAPIC page: it must be reachable through the physical map, and
/// it is made uncached there.
fn map_xapic(phys: u64) -> Result<(), &'static str> {
    if phys == 0 || !mem::mmio::phys_mapped(phys, 4096) {
        return Err("xAPIC page not mapped");
    }
    if !mem::mmio::uncache_phys_map(phys) {
        crate::serial_println!("lapic: could not make {phys:#x} uncached (left to the MTRRs)");
    }
    MMIO.store(
        mem::phys_to_virt(PhysAddr::new(phys)).as_u64(),
        Ordering::Relaxed,
    );
    Ok(())
}

/// The APIC ID (x2APIC: all 32 bits; xAPIC: bits 24..32).
pub fn id() -> u32 {
    match mode() {
        Some(Mode::X2Apic) => read(ID),
        _ => read(ID) >> 24,
    }
}

/// Which register interface is in use.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn current_mode() -> Option<Mode> {
    mode()
}

/// Acknowledge the APIC timer interrupt. Not for 8259 (ExtINT) interrupts.
pub fn eoi() {
    write(EOI, 0);
}

/// Start the timer counting down from `u32::MAX`, masked and one-shot, so
/// [`remaining`] can be sampled for calibration.
pub fn start_free_run() {
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, LVT_MASKED | u32::from(TIMER_VECTOR));
    write(TIMER_INITIAL, u32::MAX);
}

/// The timer's current count.
pub fn remaining() -> u32 {
    read(TIMER_CURRENT)
}

/// Run the timer periodically every `count` divided clocks, unmasked.
pub fn start_periodic(count: u32) {
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(LVT_TIMER, LVT_PERIODIC | u32::from(TIMER_VECTOR));
    write(TIMER_INITIAL, count.max(1));
}

/// Set the timer up as a one-shot on `vector`, unmasked and stopped: each
/// [`set_initial_count`] then raises one interrupt when it counts down.
pub fn start_oneshot(vector: u8) {
    write(TIMER_DIVIDE, DIVIDE_BY_16);
    write(TIMER_INITIAL, 0);
    write(LVT_TIMER, u32::from(vector));
}

/// Start the timer counting down from `count` divided clocks (0 stops it).
pub fn set_initial_count(count: u32) {
    write(TIMER_INITIAL, count);
}

/// Mask or unmask the timer's interrupt; the count keeps running either way.
pub fn set_timer_masked(masked: bool) {
    let lvt = read(LVT_TIMER);
    let lvt = if masked {
        lvt | LVT_MASKED
    } else {
        lvt & !LVT_MASKED
    };
    write(LVT_TIMER, lvt);
}

/// Whether the timer's interrupt is masked.
pub fn timer_masked() -> bool {
    read(LVT_TIMER) & LVT_MASKED != 0
}

/// The timer's initial count (the period while it runs periodically).
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn period() -> u32 {
    read(TIMER_INITIAL)
}
