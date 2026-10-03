//! Kernel resource limits: the one place every tunable ceiling is defined.
//!
//! LazyOS used to scatter fixed constants through the kernel (a 16 MiB heap,
//! 16 descriptors per task, a 1 MiB Linux stack, a 256 MiB user quota...),
//! sized for a 256 MiB QEMU guest. Here each limit is
//!
//! 1. **derived from the machine** ([`Limits::for_machine`]): the usable RAM
//!    the memory map reports (and, for display buffers, the screen size), so a 1 GiB PC and a 16 GiB one both get sensible
//!    ceilings without configuration;
//! 2. **overridable at boot** by a `limit.<key>=<value>` line in `lazyos.cfg`
//!    on the FAT `/boot` volume (the build writes them from `LAZYOS_LIMIT_*`
//!    variables). The file is untrusted input, so [`parse`] never rejects the
//!    whole file over one bad line: a malformed value keeps the derived
//!    default, an out-of-range one is clamped, and each outcome is logged;
//! 3. **read lock-free** through atomics. The heap's growth path reads
//!    [`heap_max`] from inside the allocator with interrupts off, so a lock
//!    here could deadlock against a preempted reader (the issue #382 class).
//!
//! Some limits are fixed before the config file can be read (the boot volume
//! is mounted with a heap that already exists): the initial heap size and the
//! DMA pool are derived from RAM only. [`describe`] prints the whole table at
//! boot with where each value came from.
//!
//! The table of keys is [`KEYS`]; `docs/architecture/limits.md` documents it
//! for users.

use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

mod derive;
mod parse;

pub use derive::{dma_pool_bytes, heap_initial_bytes, Limits};
#[cfg_attr(not(lazyos_tests), allow(unused_imports))]
pub use parse::{parse, parse_size, Outcome, Override, PREFIX};

/// One mebibyte.
pub const MIB: u64 = 1 << 20;
/// One gibibyte.
pub const GIB: u64 = 1 << 30;

/// A configurable limit: its `lazyos.cfg` key (after `limit.`) and the
/// inclusive range a value is clamped into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Key {
    pub name: &'static str,
    pub min: u64,
    pub max: u64,
    /// Values are byte sizes (accept `K`/`M`/`G` suffixes, page-aligned).
    pub bytes: bool,
}

/// Index of each key in [`KEYS`] and in the live value table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Id {
    /// Most bytes the kernel heap may grow to.
    HeapMax,
    /// Descriptors one task may hold (Linux `RLIMIT_NOFILE`).
    FdMax,
    /// Largest Linux main-thread stack (Linux `RLIMIT_STACK`).
    StackSize,
    /// Per-uid user memory (mappings, `brk`, `sbrk`) for non-root users.
    QuotaUserMemory,
    /// Per-uid kernel memory (shared buffers) for non-root users.
    QuotaKernelMemory,
    /// Shared-buffer bytes one process may hold at once.
    SharedBufferMax,
}

/// Number of configurable limits.
pub const COUNT: usize = 6;

/// Every configurable limit. The ranges are the sanity bounds a config value
/// is clamped into, not the defaults (see [`Limits::for_machine`]).
pub const KEYS: [Key; COUNT] = [
    // Below 16 MiB the boot-time heap could not even hold the console pixmap;
    // above 512 GiB the heap would leave the one PML4 entry it is shared
    // through (`mem::heap`).
    Key {
        name: "heap_max",
        min: 16 * MIB,
        max: 512 * GIB,
        bytes: true,
    },
    // 64 keeps every shell and service working; 1 Mi descriptors is far past
    // anything a desktop opens and keeps a full table's heap cost bounded.
    Key {
        name: "fd_max",
        min: 64,
        max: 1 << 20,
        bytes: false,
    },
    // musl needs a few pages for its start-up; 1 GiB is a quarter of the
    // gap the layout leaves between the mmap area and the stack top.
    Key {
        name: "stack_size",
        min: 64 * 1024,
        max: GIB,
        bytes: true,
    },
    Key {
        name: "quota_user_memory",
        min: 16 * MIB,
        max: 1 << 40,
        bytes: true,
    },
    Key {
        name: "quota_kernel_memory",
        min: 4 * MIB,
        max: 1 << 40,
        bytes: true,
    },
    // At least one full-HD RGBA frame; at most the largest buffer the
    // shared-buffer layer accepts times its per-process buffer count.
    Key {
        name: "shared_buffer_max",
        min: 8 * MIB,
        max: 64 * GIB,
        bytes: true,
    },
];

/// Where the live value of a limit came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Source {
    /// Derived from RAM (or the built-in default before RAM is known).
    Default = 0,
    /// Taken from `lazyos.cfg` as written.
    Config = 1,
    /// Taken from `lazyos.cfg` but clamped into the key's range.
    Clamped = 2,
}

/// The live values, indexed by [`Id`]. Before [`init_for_machine`] runs they hold
/// the 256 MiB defaults, so early boot code always reads a sane value.
static VALUES: [AtomicU64; COUNT] = {
    let base = Limits::BASELINE;
    [
        AtomicU64::new(base.heap_max),
        AtomicU64::new(base.fd_max),
        AtomicU64::new(base.stack_size),
        AtomicU64::new(base.quota_user_memory),
        AtomicU64::new(base.quota_kernel_memory),
        AtomicU64::new(base.shared_buffer_max),
    ]
};
static SOURCES: [AtomicU8; COUNT] = [const { AtomicU8::new(Source::Default as u8) }; COUNT];
/// Usable RAM the defaults were derived from (0 before [`init_for_machine`]).
static RAM: AtomicU64 = AtomicU64::new(0);
/// Bytes of one screen-sized RGBA surface (0 before [`init_for_machine`]).
static SCREEN: AtomicU64 = AtomicU64::new(0);

/// The live value of one limit.
pub fn get(id: Id) -> u64 {
    VALUES[id as usize].load(Ordering::Relaxed)
}

/// Where one limit's live value came from.
#[cfg_attr(not(lazyos_tests), allow(dead_code))]
pub fn source(id: Id) -> Source {
    match SOURCES[id as usize].load(Ordering::Relaxed) {
        1 => Source::Config,
        2 => Source::Clamped,
        _ => Source::Default,
    }
}

/// Usable RAM in bytes, as the memory map reported it at boot.
pub fn ram_bytes() -> u64 {
    RAM.load(Ordering::Relaxed)
}

/// Most bytes the kernel heap may grow to.
pub fn heap_max() -> u64 {
    get(Id::HeapMax)
}

/// Descriptors one task may hold.
pub fn fd_max() -> usize {
    get(Id::FdMax) as usize
}

/// Largest Linux main-thread stack, page aligned.
pub fn stack_size() -> u64 {
    get(Id::StackSize)
}

/// Shared-buffer bytes one process may hold at once.
pub fn shared_buffer_max() -> u64 {
    get(Id::SharedBufferMax)
}

/// Install the machine-derived defaults: `ram` is the usable RAM the memory
/// map reported, `screen` the bytes of one framebuffer-sized RGBA surface.
/// `kernel_main` calls this right after `mem::init`, before anything sizes
/// itself from a limit.
pub fn init_for_machine(ram: u64, screen: u64) {
    RAM.store(ram, Ordering::Relaxed);
    SCREEN.store(screen, Ordering::Relaxed);
    store_all(&Limits::for_machine(ram, screen));
}

/// Replace every live value with `limits`, all marked [`Source::Default`].
fn store_all(limits: &Limits) {
    for (index, value) in limits.values().into_iter().enumerate() {
        VALUES[index].store(value, Ordering::Relaxed);
        SOURCES[index].store(Source::Default as u8, Ordering::Relaxed);
    }
}

/// Apply the `limit.*` lines of a `lazyos.cfg` text on top of the current
/// values. Every line's outcome is logged; nothing here can fail the boot.
pub fn apply_config(text: &str) {
    for item in parse(text) {
        match item {
            Outcome::Set(over) => {
                let value = sanitize(over.id, over.value);
                VALUES[over.id as usize].store(value, Ordering::Relaxed);
                let source = if over.clamped {
                    Source::Clamped
                } else {
                    Source::Config
                };
                SOURCES[over.id as usize].store(source as u8, Ordering::Relaxed);
                if over.clamped {
                    serial_println!(
                        "limits: {} clamped to {} (allowed {}..={})",
                        KEYS[over.id as usize].name,
                        value,
                        KEYS[over.id as usize].min,
                        KEYS[over.id as usize].max
                    );
                }
            }
            Outcome::Unknown(key) => serial_println!("limits: unknown key limit.{key} ignored"),
            Outcome::Malformed(key) => {
                serial_println!("limits: bad value for limit.{key}, default kept")
            }
            Outcome::Duplicate(key) => {
                serial_println!("limits: limit.{key} repeated, the last value wins")
            }
        }
    }
    // The heap may never be asked to stay smaller than it already is.
    let floor = crate::mem::heap_stats().total as u64;
    if get(Id::HeapMax) < floor {
        VALUES[Id::HeapMax as usize].store(floor, Ordering::Relaxed);
        SOURCES[Id::HeapMax as usize].store(Source::Clamped as u8, Ordering::Relaxed);
        serial_println!("limits: heap_max raised to the mapped heap ({floor} bytes)");
    }
}

/// Cross-limit rules a lone value cannot know: byte sizes are page aligned.
fn sanitize(id: Id, value: u64) -> u64 {
    if KEYS[id as usize].bytes {
        value & !0xfff
    } else {
        value
    }
}

/// Print the live table, one line per limit with its origin, plus the values
/// that are derived only.
pub fn describe() {
    let ram = ram_bytes();
    serial_println!(
        "limits: ram={} MiB heap_initial={} MiB dma_pool={} MiB",
        ram / MIB,
        heap_initial_bytes(ram) / MIB,
        dma_pool_bytes(ram) / MIB
    );
    for (index, key) in KEYS.iter().enumerate() {
        let source = match SOURCES[index].load(Ordering::Relaxed) {
            1 => "lazyos.cfg",
            2 => "lazyos.cfg, clamped",
            _ => "default",
        };
        let (value, unit) = scaled(*key, VALUES[index].load(Ordering::Relaxed));
        serial_println!("limits: {}={value}{unit} ({source})", key.name);
    }
}

/// A value in the largest unit that keeps it whole, in the config's syntax.
fn scaled(key: Key, value: u64) -> (u64, &'static str) {
    if !key.bytes || value == 0 {
        return (value, "");
    }
    [(GIB, "G"), (MIB, "M"), (1024, "K")]
        .into_iter()
        .find(|(unit, _)| value.is_multiple_of(*unit))
        .map_or((value, ""), |(unit, suffix)| (value / unit, suffix))
}

/// Test hook: restore the RAM-derived defaults after a test applied a config.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    store_all(&Limits::for_machine(
        ram_bytes(),
        SCREEN.load(Ordering::Relaxed),
    ));
}

/// Test hook: set one live value directly (clamped like a config value).
#[cfg(lazyos_tests)]
pub fn set_for_test(id: Id, value: u64) {
    let key = KEYS[id as usize];
    VALUES[id as usize].store(value.clamp(key.min, key.max), Ordering::Relaxed);
}
