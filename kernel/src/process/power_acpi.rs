//! ACPI soft-off (S5): how `power(SHUTDOWN)` turns a real PC off
//! (docs/shutdown.md, docs/real-pc-boot-plan.md H4).
//!
//! The plan is resolved from the firmware tables alone ([`plan`]: the FADT's
//! PM1 control blocks, or the sleep control register of a hardware-reduced
//! platform, and the `\_S5` sleep type from a bounded scan of the DSDT), so it
//! can be checked and logged before anything is written. [`Plan::enter`] is
//! the write sequence Linux uses: `SLP_TYP`, then `SLP_EN`, in PM1a and PM1b.
//! QEMU describes the same thing (PM1a control at 0x604, type 0), so the
//! emulator takes the same path as hardware.

use alloc::format;
use alloc::string::String;

use ::acpi::s5::{pm1_control_writes, sleep_control_value};
use ::acpi::{AddressSpace, Gas, SleepType};
use x86_64::PhysAddr;

use crate::arch::acpi_tables;
use crate::arch::io::{inw, outb, outw};
use crate::mem;

/// Where a power-management register lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reg {
    /// An I/O port.
    Port(u16),
    /// A memory-mapped register: its virtual address in the physical map.
    Mem(u64),
}

impl Reg {
    /// The register `gas` describes, `bytes` wide, when the kernel can reach
    /// it: an I/O port inside the 16-bit space or memory inside the physical
    /// map. Anything else (PCI configuration, embedded controller, ...) is
    /// never touched.
    fn of(gas: Gas, bytes: u16) -> Option<Reg> {
        match gas.space {
            AddressSpace::Io => gas.port(bytes).map(Reg::Port),
            AddressSpace::Memory if mem::mmio::phys_mapped(gas.address, u64::from(bytes)) => {
                let va = mem::phys_to_virt(PhysAddr::new(gas.address)).as_u64();
                Some(Reg::Mem(va))
            }
            _ => None,
        }
    }

    /// # Safety
    /// The register must be a PM1 control register (16 bits wide).
    #[cfg_attr(lazyos_tests, allow(dead_code))] // the suite resolves, never writes
    unsafe fn read16(self) -> u16 {
        match self {
            Reg::Port(port) => inw(port),
            Reg::Mem(va) => core::ptr::read_volatile(va as *const u16),
        }
    }

    /// # Safety
    /// The register must be a PM1 control register (16 bits wide).
    #[cfg_attr(lazyos_tests, allow(dead_code))]
    unsafe fn write16(self, value: u16) {
        match self {
            Reg::Port(port) => outw(port, value),
            Reg::Mem(va) => core::ptr::write_volatile(va as *mut u16, value),
        }
    }

    /// # Safety
    /// The register must be the FADT's sleep control register (8 bits wide).
    #[cfg_attr(lazyos_tests, allow(dead_code))]
    unsafe fn write8(self, value: u8) {
        match self {
            Reg::Port(port) => outb(port, value),
            Reg::Mem(va) => core::ptr::write_volatile(va as *mut u8, value),
        }
    }
}

/// How to enter S5 on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    /// PM1a (and PM1b) control: the fixed-hardware PC.
    Pm1 {
        sleep: SleepType,
        a: Reg,
        b: Option<Reg>,
    },
    /// The sleep control register of hardware-reduced ACPI.
    SleepControl { sleep: SleepType, reg: Reg },
}

/// Resolve the plan from the firmware tables; the error says what is missing
/// (it goes into the boot log, so a board that cannot power off says why).
pub fn plan() -> Result<Plan, String> {
    let platform = acpi_tables::platform().ok_or("no ACPI tables")?;
    let fadt = platform
        .fadt
        .as_ref()
        .map_err(|error| format!("no usable FADT ({error:?})"))?;
    let sleep = acpi_tables::find_s5().map_err(|error| format!("no usable \\_S5 ({error:?})"))?;
    if fadt.hw_reduced() {
        let reg = fadt
            .sleep_control
            .and_then(|gas| Reg::of(gas, 1))
            .ok_or("hardware-reduced ACPI without a usable sleep control register")?;
        return Ok(Plan::SleepControl { sleep, reg });
    }
    let a = fadt
        .pm1a_control
        .and_then(|gas| Reg::of(gas, 2))
        .ok_or("no usable PM1a control block")?;
    // PM1b is optional; a block that exists but cannot be reached is skipped
    // rather than refusing the power-off PM1a alone can perform.
    let b = fadt.pm1b_control.and_then(|gas| Reg::of(gas, 2));
    Ok(Plan::Pm1 { sleep, a, b })
}

impl Plan {
    /// One log line: the sleep type and the registers it is written to.
    pub fn describe(&self) -> String {
        match self {
            Plan::Pm1 { sleep, a, b } => {
                format!("slp_typ={}/{} PM1a={:?} PM1b={:?}", sleep.a, sleep.b, a, b)
            }
            Plan::SleepControl { sleep, reg } => {
                format!("slp_typ={} sleep_control={:?}", sleep.a, reg)
            }
        }
    }

    /// Write the registers that enter S5. Returns if the platform does not
    /// power off (the caller then falls back); interrupts must be off.
    ///
    /// # Safety
    /// Stops the machine: only for a `CAP_SYS_ADMIN` caller's power-off (or
    /// the watchdog acting for one), after the filesystems are synced.
    #[cfg_attr(lazyos_tests, allow(dead_code))] // the suite proves the plan only
    pub unsafe fn enter(&self) {
        match *self {
            Plan::Pm1 { sleep, a, b } => {
                // (register, SLP_TYP, value with SLP_TYP, value with SLP_EN).
                let mut blocks = [(a, sleep.a, 0u16, 0u16), (a, sleep.b, 0, 0)];
                let mut count = 1;
                if let Some(b) = b {
                    blocks[1].0 = b;
                    count = 2;
                }
                let blocks = &mut blocks[..count];
                for block in blocks.iter_mut() {
                    let (prepared, enter) = pm1_control_writes(block.0.read16(), block.1);
                    block.2 = prepared;
                    block.3 = enter;
                }
                // SLP_TYP first in both blocks, then SLP_EN in both.
                for block in blocks.iter() {
                    block.0.write16(block.2);
                }
                for block in blocks.iter() {
                    block.0.write16(block.3);
                }
            }
            Plan::SleepControl { sleep, reg } => reg.write8(sleep_control_value(sleep.a)),
        }
    }
}
