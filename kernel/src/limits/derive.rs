//! Defaults derived from the machine: pure functions of RAM and screen size,
//! so the test suite can check them for any machine shape.

use super::{COUNT, GIB, KEYS, MIB};

/// One full set of configurable limits, in [`super::Id`] order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub heap_max: u64,
    pub fd_max: u64,
    pub stack_size: u64,
    pub quota_user_memory: u64,
    pub quota_kernel_memory: u64,
    pub shared_buffer_max: u64,
    pub scratch_max: u64,
    pub task_max: u64,
}

impl Limits {
    /// What a 256 MiB guest with a 1280x720 screen gets: the values the
    /// kernel uses before the memory map has been read.
    pub const BASELINE: Limits = Limits {
        heap_max: 64 * MIB,
        fd_max: 8192,
        stack_size: 8 * MIB,
        quota_user_memory: 256 * MIB,
        quota_kernel_memory: 32 * MIB,
        shared_buffer_max: 16 * MIB,
        scratch_max: 32 * MIB,
        task_max: 256,
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
        let heap_max = down(ram / 2).max(heap_initial_bytes(ram));
        let limits = Limits {
            // Half of RAM: the heap holds ramfs files, file snapshots, the
            // console pixmap and every kernel object, but user memory needs
            // the other half. It only grows on demand.
            heap_max,
            // Chromium raises its soft limit to 8192; the table grows on
            // demand, so a high ceiling costs heap only for the descriptors
            // a task really opens (a slot is 32 bytes).
            fd_max: base.fd_max,
            // Linux's default `ulimit -s`; pages are demand-zero.
            stack_size: base.stack_size,
            // Charged on reservation (mappings, `brk`), not residency, so it
            // may exceed what is resident; three quarters of RAM keeps one
            // runaway user from reserving everything. Never more than the
            // frames a user could be given: on the smallest machine the
            // 256 MiB floor is all of RAM, a promise nothing could keep.
            quota_user_memory: down(ram / 4 * 3)
                .max(base.quota_user_memory)
                .min(user_memory_ceiling(ram)),
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
            // A package staged in `/transient` is the largest thing the
            // scratch area holds (the LazyRAD player alone is 7 MiB), and
            // the files live in the kernel heap: 1 GiB, but never more than
            // half the heap ceiling, so a full `/tmp` leaves the kernel
            // room for its own objects.
            scratch_max: GIB.min(down(heap_max / 2)).max(base.scratch_max),
            // One slot per MiB of RAM: a task costs a 48 KiB kernel stack
            // and its page tables, so 256 on the smallest machine, the
            // table's 1024 from 1 GiB.
            task_max: (ram / MIB).max(base.task_max),
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
            self.scratch_max,
            self.task_max,
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
        let [heap_max, fd_max, stack_size, quota_user_memory, quota_kernel_memory, shared_buffer_max, scratch_max, task_max] =
            values;
        Limits {
            heap_max,
            fd_max,
            stack_size,
            quota_user_memory,
            quota_kernel_memory,
            shared_buffer_max,
            scratch_max,
            task_max,
        }
    }
}

/// The most user memory a default quota may promise: RAM less what the kernel
/// takes at boot (the initial heap and the DMA pool, [`heap_initial_bytes`]
/// and [`dma_pool_bytes`]). A configured quota may go past it (a program that
/// reserves more than it touches needs the room, and the page fault answers a
/// real shortage with an OOM kill), but `limits::apply_config` says so.
pub fn user_memory_ceiling(ram: u64) -> u64 {
    ram.saturating_sub(heap_initial_bytes(ram) + dma_pool_bytes(ram))
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
