//! Reference clocks the tick source is chosen and calibrated with: the ACPI
//! PM timer and the HPET main counter (both read-only free-running
//! counters), plus the probes of the 8254 PIT itself.
//!
//! Every wait here is bounded twice: by the reference clock's own elapsed
//! time and by a spin count, so a counter that stops (or never ran) ends the
//! wait instead of hanging boot.

use ::acpi::fadt::PM_TIMER_HZ;
use ::acpi::{hpet, AddressSpace, Platform};
use x86_64::PhysAddr;

use super::io::{inb, inl, outb};
use crate::mem;

/// A free-running counter of known frequency.
#[derive(Clone, Copy, Debug)]
pub enum RefClock {
    /// ACPI PM timer in I/O space (3.579545 MHz); `mask` is its width.
    PmTimerIo { port: u16, mask: u32 },
    /// ACPI PM timer in memory space.
    PmTimerMmio { va: u64, mask: u32 },
    /// HPET main counter; `va` is the register block.
    Hpet { va: u64, hz: u64, mask: u64 },
}

impl RefClock {
    pub fn name(&self) -> &'static str {
        match self {
            RefClock::PmTimerIo { .. } | RefClock::PmTimerMmio { .. } => "pmtimer",
            RefClock::Hpet { .. } => "hpet",
        }
    }

    pub fn hz(&self) -> u64 {
        match self {
            RefClock::Hpet { hz, .. } => *hz,
            _ => PM_TIMER_HZ,
        }
    }

    fn mask(&self) -> u64 {
        match *self {
            RefClock::PmTimerIo { mask, .. } | RefClock::PmTimerMmio { mask, .. } => {
                u64::from(mask)
            }
            RefClock::Hpet { mask, .. } => mask,
        }
    }

    /// The raw counter.
    pub fn read(&self) -> u64 {
        match *self {
            // SAFETY: the FADT's PM timer port, validated by `libs/acpi` to fit
            // the I/O space; reading it has no side effects.
            RefClock::PmTimerIo { port, mask } => u64::from(unsafe { inl(port) } & mask),
            RefClock::PmTimerMmio { va, mask } => {
                // SAFETY: `find` checked the 4-byte register is mapped.
                u64::from(unsafe { (va as *const u32).read_volatile() } & mask)
            }
            RefClock::Hpet { va, mask, .. } => {
                let at = (va + hpet::reg::MAIN_COUNTER) as *const u64;
                // SAFETY: `find` mapped the HPET block uncached.
                unsafe { at.read_volatile() & mask }
            }
        }
    }

    /// Counter ticks from `earlier` to `later`, allowing one wrap.
    pub fn delta(&self, earlier: u64, later: u64) -> u64 {
        later.wrapping_sub(earlier) & self.mask()
    }

    /// Whether the counter moves within a short bounded spin.
    fn counts(&self) -> bool {
        let first = self.read();
        (0..100_000).any(|_| self.read() != first)
    }
}

/// A running interval on a reference clock, accumulated across wraps (the
/// 24-bit PM timer wraps every 4.7 s; callers poll far more often).
pub struct Stopwatch {
    clock: RefClock,
    last: u64,
    ticks: u64,
}

impl Stopwatch {
    pub fn start(clock: RefClock) -> Stopwatch {
        Stopwatch {
            clock,
            last: clock.read(),
            ticks: 0,
        }
    }

    /// Nanoseconds since [`Stopwatch::start`].
    pub fn elapsed_ns(&mut self) -> u64 {
        let now = self.clock.read();
        self.ticks += self.clock.delta(self.last, now);
        self.last = now;
        (u128::from(self.ticks) * 1_000_000_000 / u128::from(self.clock.hz())) as u64
    }
}

/// The best reference clock the firmware describes that is actually
/// counting: the PM timer, else the HPET. The test switch
/// `LAZYOS_TIMER_REF=hpet` skips the PM timer and `none` uses neither, so
/// QEMU (which always has a PM timer) can exercise the other calibrations.
pub fn find(platform: Option<&Platform>) -> Option<RefClock> {
    let platform = platform?;
    let only = option_env!("LAZYOS_TIMER_REF").unwrap_or("");
    if only == "none" {
        return None;
    }
    let pm = (only != "hpet")
        .then(|| platform.fadt.as_ref().ok().and_then(|f| f.pm_timer))
        .flatten();
    if let Some(timer) = pm {
        let mask = timer.mask();
        let clock = match timer.block.space {
            AddressSpace::Io => timer
                .block
                .port(4)
                .map(|port| RefClock::PmTimerIo { port, mask }),
            AddressSpace::Memory if mem::mmio::phys_mapped(timer.block.address, 4) => {
                let va = mem::phys_to_virt(PhysAddr::new(timer.block.address)).as_u64();
                Some(RefClock::PmTimerMmio { va, mask })
            }
            _ => None,
        };
        match clock {
            Some(clock) if clock.counts() => return Some(clock),
            _ => crate::serial_println!("timer: the FADT's PM timer does not count"),
        }
    }
    let table = platform.hpet.as_ref().ok()?;
    let clock = hpet_clock(table.address, table.counter64())?;
    if clock.counts() {
        Some(clock)
    } else {
        crate::serial_println!("timer: the HPET main counter does not count");
        None
    }
}

/// Map the HPET block uncached, check its period, and start its main counter
/// if firmware left it stopped (comparator interrupts stay as they are).
fn hpet_clock(phys: u64, counter64: bool) -> Option<RefClock> {
    if !mem::mmio::phys_mapped(phys, hpet::BLOCK_LEN) || !mem::mmio::uncache_phys_map(phys) {
        return None;
    }
    let va = mem::phys_to_virt(PhysAddr::new(phys)).as_u64();
    let reg = |offset: u64| (va + offset) as *mut u64;
    // SAFETY: the block is mapped uncached (above); these are the HPET's
    // 64-bit capability and configuration registers.
    let (caps, config) = unsafe {
        (
            reg(hpet::reg::CAPABILITIES).read_volatile(),
            reg(hpet::reg::CONFIG).read_volatile(),
        )
    };
    let hz = hpet::frequency(hpet::period_fs(caps)?);
    if config & hpet::reg::LEG_RT_CNF != 0 {
        crate::serial_println!("timer: HPET legacy replacement is on (IRQ0 is the HPET's)");
    }
    if config & hpet::reg::ENABLE_CNF == 0 {
        // SAFETY: as above; setting ENABLE_CNF only starts the main counter.
        unsafe { reg(hpet::reg::CONFIG).write_volatile(config | hpet::reg::ENABLE_CNF) };
    }
    let mask = if counter64 {
        u64::MAX
    } else {
        u64::from(u32::MAX)
    };
    Some(RefClock::Hpet { va, hz, mask })
}

/// Latch and read PIT channel 0's current count.
pub fn pit_count() -> u16 {
    // SAFETY: the counter-latch command for channel 0 (0x00 to 0x43) and the
    // two data-port reads that follow change no counting state; the kernel
    // owns the PIT and interrupts are off during the boot probes.
    unsafe {
        outb(0x43, 0x00);
        let lo = inb(0x40);
        let hi = inb(0x40);
        u16::from_le_bytes([lo, hi])
    }
}
