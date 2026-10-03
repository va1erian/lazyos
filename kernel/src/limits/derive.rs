//! Defaults derived from the machine: pure functions of RAM and screen size,
//! so the test suite can check them for any machine shape.

use super::{COUNT, KEYS, MIB};

/// One full set of configurable limits, in [`super::Id`] order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub heap_max: u64,
    pub fd_max: u64,
    pub stack_size: u64,
    pub quota_user_memory: u64,
    pub quota_kernel_memory: u64,
    pub shared_buffer_max: u64,
}

impl Limits {
    /// What a 256 MiB guest with a 1280x720 screen gets: the values the
    /// kernel uses before the memory map has been read.
    pub const BASELINE: Limits = Limits {
        heap_max: 64 * MIB,
        fd_max: 1024,
        stack_size: 8 * MIB,
        quota_user_memory: 256 * MIB,
        quota_kernel_memory: 32 * MIB,
        shared_buffer_max: 16 * MIB,
    };

    /// The defaults for a machine with `ram` usable bytes and a screen whose
    /// RGBA surface is `screen` bytes. Every value lands inside its key's
    /// range, so a derived default never needs clamping.
    pub fn for_machine(ram: u64, screen: u64) -> Limits {
        let base = Limits::BASELINE;
        // Whole mebibytes, so the boot log and the docs read cleanly: RAM
        // shares round down, screen-driven room rounds up.
        let down = |bytes: u64| bytes & !(MIB - 1);
        let up = |bytes: u64| bytes.saturating_add(MIB - 1) & !(MIB - 1);
        let limits = Limits {
            // Half of RAM: the heap holds ramfs files, file snapshots, the
            // console pixmap and every kernel object, but user memory needs
            // the other half. It only grows on demand.
            heap_max: down(ram / 2).max(heap_initial_bytes(ram)),
            // Linux's default soft limit; the table grows on demand, so a
            // high ceiling costs nothing until a task opens that many.
            fd_max: base.fd_max,
            // Linux's default `ulimit -s`; pages are demand-zero.
            stack_size: base.stack_size,
            // Charged on reservation (mappings, `brk`), not residency, so it
            // may exceed what is resident; three quarters of RAM keeps one
            // runaway user from reserving everything.
            quota_user_memory: down(ram / 4 * 3).max(base.quota_user_memory),
            // Shared buffers: a desktop holds a few screen-sized surfaces
            // per window, so the quota follows the screen as well as RAM.
            quota_kernel_memory: up(down(ram / 8).max(screen.saturating_mul(8)))
                .max(base.quota_kernel_memory)
                .min(down(ram / 2).max(base.quota_kernel_memory)),
            // A double-buffered full-screen window plus the compositor's
            // screen buffer, never more than a quarter of RAM.
            shared_buffer_max: up(screen.saturating_mul(3))
                .max(base.shared_buffer_max)
                .min(down(ram / 4).max(base.shared_buffer_max)),
        };
        limits.clamped()
    }

    /// The values in [`super::Id`] order.
    pub fn values(&self) -> [u64; COUNT] {
        [
            self.heap_max,
            self.fd_max,
            self.stack_size,
            self.quota_user_memory,
            self.quota_kernel_memory,
            self.shared_buffer_max,
        ]
    }

    /// Every value forced into its key's range (and byte sizes page aligned).
    fn clamped(self) -> Limits {
        let mut values = self.values();
        for (value, key) in values.iter_mut().zip(KEYS.iter()) {
            *value = (*value).clamp(key.min, key.max);
            if key.bytes {
                *value &= !0xfff;
            }
        }
        let [heap_max, fd_max, stack_size, quota_user_memory, quota_kernel_memory, shared_buffer_max] =
            values;
        Limits {
            heap_max,
            fd_max,
            stack_size,
            quota_user_memory,
            quota_kernel_memory,
            shared_buffer_max,
        }
    }
}

/// The heap mapped at boot, before `lazyos.cfg` can be read: 1/32 of RAM,
/// between 16 MiB (what the kernel always had) and 64 MiB. Growth covers the
/// rest, so this only saves the first few growth steps.
/// Page aligned.
pub fn heap_initial_bytes(ram: u64) -> u64 {
    (ram / 32).clamp(16 * MIB, 64 * MIB) & !0xfff
}

/// The DMA pool reserved at boot: 1/32 of RAM, at least 16 MiB but never more
/// than 1/8 of RAM (so a small guest keeps most of its memory), and at most
/// 64 MiB (the pool bitmap's size). Page aligned.
pub fn dma_pool_bytes(ram: u64) -> u64 {
    let wanted = (ram / 32).clamp(16 * MIB, 64 * MIB);
    wanted.min(ram / 8) & !0xfff
}
