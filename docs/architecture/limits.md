# Resource limits

**What it is.** Every tunable kernel ceiling in one place
(`kernel/src/limits.rs`): defaults derived from the machine (usable RAM and the
screen size), overridable at boot through `lazyos.cfg`, read lock-free by the
code each limit governs. The limits that cannot be configured (fixed at
compile time, or fixed before the config file is readable) are listed at the
end with the reason.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/limits.rs` | The key table (`KEYS`, ranges), live values (atomics), `apply_config`, `describe` |
| `kernel/src/limits/derive.rs` | `Limits::for_machine(ram, screen)`, `heap_initial_bytes`, `dma_pool_bytes` |
| `kernel/src/limits/parse.rs` | The `limit.<key>=<value>` parser (pure; hostile-input tested) |
| `kernel/src/fs/bootcfg.rs` | Reads `lazyos.cfg`, hands its text to `limits::apply_config`, skips `limit.` lines itself |
| `build_support/limits_cfg.rs` | `LAZYOS_LIMIT_<KEY>` build variables -> `limit.*` lines in the generated `lazyos.cfg` |
| `kernel/src/tests/limits_suite.rs` | Parsing (hostile values, fuzz), clamping, defaults for every machine shape |

## How a value is chosen

1. `mem::init` reads the memory map; `kernel_main` then calls
   `limits::init_for_machine(usable RAM, screen bytes)` before anything sizes
   itself from a limit.
2. When the FAT `/boot` volume is mounted, `fs::bootcfg::load` passes the
   text of `lazyos.cfg` to `limits::apply_config`. Each `limit.<key>=<value>`
   line is parsed on its own: a malformed value keeps the default, an
   out-of-range one is clamped into the key's range, an unknown key or a
   repeated one is reported; nothing here can fail the boot or cost the root
   volume (`bootcfg` ignores `limit.` lines, so its own all-or-nothing parse
   never sees them).
3. `limits::describe` prints the table at boot, one line per key with its
   source (`default`, `lazyos.cfg`, `lazyos.cfg, clamped`):

```text
limits: ram=1011 MiB heap_initial=31 MiB dma_pool=31 MiB
limits: heap_max=505M (default)
limits: fd_max=1024 (lazyos.cfg)
limits: stack_size=1G (lazyos.cfg, clamped)
...
```

Derived values are whole MiB (RAM shares rounded down, screen room up).
Values are decimal; byte sizes take an optional binary suffix `K`, `M`, `G`
or `T` (either case) and are rounded down to a page. Readers use atomics, not
a lock: the heap reads `heap_max` from inside the allocator with interrupts
off, where a lock could deadlock against a preempted holder (the #382 class).

## The configurable limits

| Key | Governs | Default (RAM `r`, screen surface `s` bytes) | Range |
|---|---|---|---|
| `heap_max` | Kernel heap ceiling (`mem::heap` grows on demand up to it) | `r / 2` | 16 MiB .. 512 GiB |
| `fd_max` | Descriptors per task (`RLIMIT_NOFILE`; `task::FdTable`) | 1024 | 64 .. 1 Mi |
| `stack_size` | Linux main-thread stack (`RLIMIT_STACK`) | 8 MiB | 64 KiB .. 1 GiB |
| `quota_user_memory` | Per-uid user memory (mappings, `brk`, `sbrk`), non-root | `max(256 MiB, 3r/4)` | 16 MiB .. 1 TiB |
| `quota_kernel_memory` | Per-uid kernel memory (shared buffers), non-root | `max(32 MiB, r/8, 8s)`, at most `max(32 MiB, r/2)` | 4 MiB .. 1 TiB |
| `shared_buffer_max` | Shared-buffer bytes one process holds (and the largest single buffer) | `max(16 MiB, 3s)`, at most `max(16 MiB, r/4)` | 8 MiB .. 64 GiB |

What that gives (screen 1280x720 unless stated):

| Machine | `heap_max` | `quota_user_memory` | `quota_kernel_memory` | `shared_buffer_max` | DMA pool |
|---|---|---|---|---|---|
| 256 MiB | 128 MiB | 256 MiB | 32 MiB | 16 MiB | 16 MiB |
| 1 GiB | 512 MiB | 768 MiB | 128 MiB | 16 MiB | 32 MiB |
| 1 GiB, 1920x1080 | 512 MiB | 768 MiB | 128 MiB | 24 MiB | 32 MiB |
| 1 GiB, 3840x2160 | 512 MiB | 768 MiB | 254 MiB | 95 MiB | 32 MiB |
| 8 GiB | 4 GiB | 6 GiB | 1 GiB | 16 MiB | 64 MiB |

Root (uid 0) is metered but not capped by the quotas (`quota::ROOT_LIMITS`).

**Setting them.** In `lazyos.cfg` on `/boot` (the image build writes it):

```text
limit.heap_max=768M
limit.fd_max=4096
limit.stack_size=16M
```

From the build: `LAZYOS_LIMIT_HEAP_MAX=768M LAZYOS_LIMIT_FD_MAX=4096 cargo build`
(the build refuses a malformed value), `python tools/run_demo.py --limit
heap_max=768M --limit fd_max=4096`, or the GUI launcher's Advanced tab ("Kernel
limits": `heap_max=768M fd_max=4096`).

## Derived only

These are fixed before `lazyos.cfg` can be read (the boot volume is mounted
with a heap that already exists), so they follow RAM alone:

| Value | Rule | Why |
|---|---|---|
| Initial heap | `r / 32`, 16..64 MiB | Mapped by `mem::init`; growth covers the rest |
| DMA pool | `r / 32`, at least 16 MiB, at most `r / 8` and 64 MiB | Reserved from the memory map below 4 GiB (`mem::dma`); the bitmap holds 64 MiB |
| Per-uid DMA quota | half the pool, at least 8 MiB | Follows the pool |
| Per-uid descriptor quota | `4 * fd_max` | Charged per open descriptor; `EMFILE` past it (issue #483) |

## Fixed limits (compile time)

| Limit | Value | Why it stays fixed |
|---|---|---|
| `task::MAX_TASKS` | 256 (pid == slot) | The task table, the per-slot registries (credentials, handle tables, FPU areas, argument blocks) and the kernel stacks (`KSTACKS`, 32 KiB each) are static arrays, and the task/sysinfo snapshot ABIs carry one row per slot (`libs/lazyos-sys/src/sysinfo/` mirrors it). Raising it means heap-allocated kernel stacks and an ABI version bump |
| User address space | ~127.5 TiB private window, then a 512 GiB shared-buffer window | PML4 entries 0..254 / 255 (`mem::layout`); far beyond any RAM |
| Kernel heap span | 512 GiB | One PML4 entry, so heap growth is visible in every address space |
| Usable memory regions | 128 after merging | `mem::regions`; the smallest are dropped (and logged) past that |
| Per-process live shared buffers | 64 | `ipc::shared::MAX_BUFFERS_PER_PROCESS`; the bytes are configurable |
| Linux `mmap` area | 96 TiB (`process::layout`) | Layout, not memory: quota and RAM bound use |

**Status.** Working; covered by `limits_suite`, `heap_suite` (growth, ceiling,
soak), `linux_suite::fd_table`, `loader_suite`, `mem_suite::layout` and the
build-side `limits_tests`.
