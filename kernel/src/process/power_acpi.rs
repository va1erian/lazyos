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

use ::acpi::s5::{pm1_enter_sequence, sleep_control_value, PM1_STATUS_CLEAR};
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
            AddressSpace::Memory => {
                // Firmware fields are hostile: refuse an address the CPU
                // cannot form, a misaligned one, RAM (a write would land in
                // kernel memory) and anything the physical map lacks, and
                // make the page uncached so the write reaches the device.
                let size = u64::from(bytes);
                let end = gas.address.checked_add(size)?;
                if end > mem::mmio::phys_limit()
                    || gas.address % size != 0
                    || mem::mmio::overlaps_ram(gas.address, end)
                    || !mem::mmio::phys_mapped(gas.address, size)
                    || !mem::mmio::uncache_phys_map(gas.address)
                {
                    return None;
                }
                let va = mem::phys_to_virt(PhysAddr::new(gas.address)).as_u64();
                Some(Reg::Mem(va))
            }
            _ => None,
        }
    }

    /// Short text for the boot log: a port in hex, or a mapped address.
    fn describe(self) -> String {
        match self {
            Reg::Port(port) => format!("port {port:#x}"),
            Reg::Mem(va) => format!("mmio va {va:#x}"),
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
        /// The PM1a/PM1b status registers (the first half of the event
        /// blocks), cleared before sleeping; absent ones are skipped.
        status_a: Option<Reg>,
        status_b: Option<Reg>,
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
    let status_a = fadt.pm1a_event.and_then(|gas| Reg::of(gas, 2));
    let status_b = fadt.pm1b_event.and_then(|gas| Reg::of(gas, 2));
    Ok(Plan::Pm1 {
        sleep,
        a,
        b,
        status_a,
        status_b,
    })
}

impl Plan {
    /// One log line: the sleep type and the registers it is written to.
    pub fn describe(&self) -> String {
        match self {
            Plan::Pm1 { sleep, a, b, .. } => {
                let b = b.map_or(String::from("none"), Reg::describe);
                format!(
                    "slp_typ={}/{} PM1a={} PM1b={}",
                    sleep.a,
                    sleep.b,
                    a.describe(),
                    b
                )
            }
            Plan::SleepControl { sleep, reg } => {
                format!("slp_typ={} sleep_control={}", sleep.a, reg.describe())
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
            Plan::Pm1 {
                sleep,
                a,
                b,
                status_a,
                status_b,
            } => {
                // Stale status bits (the power button press, a wake flag)
                // must not wake the machine the moment it sleeps.
                for status in [status_a, status_b].into_iter().flatten() {
                    status.write16(PM1_STATUS_CLEAR);
                }
                let mut regs = [a, a];
                let mut blocks = [(0u16, sleep.a), (0u16, sleep.b)];
                let mut count = 1;
                if let Some(b) = b {
                    regs[1] = b;
                    count = 2;
                }
                for (block, reg) in blocks.iter_mut().zip(regs).take(count) {
                    block.0 = reg.read16();
                }
                let (writes, used) = pm1_enter_sequence(&blocks[..count]);
                for &(index, value) in &writes[..used] {
                    regs[index].write16(value);
                }
            }
            Plan::SleepControl { sleep, reg } => reg.write8(sleep_control_value(sleep.a)),
        }
    }
}
