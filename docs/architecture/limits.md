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
| `kernel/src/task/kstack.rs` | Kernel stacks from the heap, the claim of a slot below `task_max`, the heap reserve |
| `kernel/src/task/fdlimit.rs` | `RLIMIT_NOFILE` per process (soft and hard, under `fd_max`) |
| `tools/abi/fixtures/src/bigapp.rs` | The thread / memory / descriptor ceiling fixture (`tools/abi/run.py --only bigapp`) |

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
| `fd_max` | Descriptors per process: the hard `RLIMIT_NOFILE`; a process may set its soft limit anywhere below (`task::FdTable`, `task/fdlimit.rs`) | 8192 | 64 .. 1 Mi |
| `stack_size` | Linux main-thread stack (`RLIMIT_STACK`) | 8 MiB | 64 KiB .. 1 GiB |
| `quota_user_memory` | Per-uid user memory (mappings by length, reservations by resident page, `brk`, `sbrk`), non-root | `max(256 MiB, 3r/4)`, at most `r` less the initial heap and the DMA pool | 16 MiB .. 1 TiB |
| `quota_kernel_memory` | Per-uid kernel memory (shared buffers), non-root | `max(32 MiB, r/8, 8s)`, at most `max(32 MiB, r/2)` | 4 MiB .. 1 TiB |
| `shared_buffer_max` | Shared-buffer bytes one process holds (and the largest single buffer) | `max(16 MiB, 3s)`, at most `max(16 MiB, r/4)` | 8 MiB .. 64 GiB |
| `scratch_max` | File bytes the in-memory scratch filesystem (`/transient` and `/tmp`, one instance) holds; a non-root uid may hold half | `min(1 GiB, heap_max / 2)`, at least 32 MiB | 4 MiB .. 64 GiB |
| `task_max` | Task slots (processes and threads) in use machine-wide; `RLIMIT_NPROC`. A slot costs a 48 KiB kernel stack from the heap only while a task holds it | one per MiB of RAM, 256 .. 1024 | 64 .. `task::MAX_TASKS` (1024) |

What that gives (screen 1280x720 unless stated):

| Machine | `heap_max` | `quota_user_memory` | `quota_kernel_memory` | `shared_buffer_max` | DMA pool |
|---|---|---|---|---|---|
| 256 MiB | 128 MiB | 224 MiB | 32 MiB | 16 MiB | 16 MiB |
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

## What one big program can hold (the `bigapp` measurements)

A WebKit2-class browser is three or four processes and dozens of threads, with
a few hundred MB resident and thousands of descriptors. Issue #774 measured
what one program gets, with a fixture and not by reading code:
`tools/abi/fixtures/src/bigapp.rs` runs as the session user (uid 1000, the one
the per-uid quotas cap) and (a) starts threads, each parked on a futex like an
idle worker, until `clone` fails, (b) maps and touches anonymous memory in
8 MiB pieces until `mmap` fails, (c) `dup`s descriptors until `EMFILE`,
(d) forks four children that each do a smaller round of (a)-(c) at once, and
(e) checks `getrlimit`/`setrlimit(RLIMIT_NOFILE)`. It prints
`ABI:bigapp:threads=<n>:mem=<MiB>:fds=<n>:PASS` (the ceilings are the
measurement: only an unclean stop fails it).

```bash
python tools/abi/build.py                                  # the fixtures (musl target)
python tools/abi/run.py --only bigapp --at 40 --memory 4G  # one row
python tools/abi/run.py --only bigapp --memory 1G --limit heap_max=32M   # --limit is passed to the image build
```

Results (QEMU/WHPX, a console image with only the fixture running, 1280x720):

| Guest RAM | Limits | Threads | Memory (MiB) | Descriptors |
|---|---|---|---|---|
| 1 GiB | before #774 (`MAX_TASKS` 256, `fd_max` 1024) | 254 | 752 | 1024 |
| 2 GiB | before | 254 | 1520 | 1024 |
| 4 GiB | before | 254 | 3056 | 1024 |
| 1 GiB | defaults | 1016 | 760 | 8192 |
| 2 GiB | defaults | 1022 | 1528 | 8192 |
| 4 GiB | defaults | 1022 | 3064 | 8192 |
| 4 GiB | `task_max=256 fd_max=1024` (the old ceilings) | 254 | 3064 | 1024 |
| 4 GiB | `fd_max=65536` | 1022 | 3064 | 65536 |
| 4 GiB | `heap_max=64M` | 1022 | 3064 | 8192 |
| 1 GiB | `heap_max=32M` (the heap the boot mapped) | 517 | 760 | 8192 |
| 1 GiB | `quota_user_memory=2G` | 1016 | killed by `SIGKILL` (oom) at ~940 | - |

How to read it:

* **Threads** stop at `task_max - 2` (the kernel task and the fixture's own
  slot): 254 at the old 256 slots, 1022 with a 1024-slot table. On a real
  desktop the boot services, `xuid`, LazyShell and the apps take their share
  of the same table first. Each live task costs 48 KiB of kernel stack from
  the heap plus about 1 KiB of task state: **49288 bytes of heap per thread**
  (measured by `task_threads_fill_the_ceiling`), no frames. A thread parked
  in `futex` needs nothing more, so 1000 blocked threads are 48 MiB of heap
  and the kernel stack is not what breaks first: the slot table is, or the
  heap if `heap_max` is tight (the `heap_max=32M` row: 517 threads, then
  `EAGAIN`, no panic: a stack is refused while the heap would drop under its
  reserve of 4 MiB or `heap_max / 16`).
* **Memory** stops at `quota_user_memory` less the program image and stacks
  (counted mappings are charged at `mmap`, so the failure is a clean `ENOMEM`,
  nothing is touched; a reservation fails at the touch, see below). `quota_user_memory` and `heap_max` scale with `--memory` as the
  table above says (4 GiB: 3 GiB and 2 GiB), and the quota is per uid, not per
  process, so a 600 MB mapping in one process plus 200 MB in a sibling fits
  at 4 GiB (3064 MiB) but not at 1 GiB (760 MiB); the four children above
  map 4 x 64 MiB at once.
  Raising the quota past what the machine has is not an `ENOMEM` any more:
  pages are demand-zero, so the quota is a promise and the allocator answers
  at the first touch of a page with no frame behind it. See "Running out of
  memory" below: the process is ended with `SIGKILL`, as Linux's OOM killer
  does, and its address space is reclaimed; nothing else is hurt.
* **Descriptors** stop at `fd_max` (`EMFILE`; `dup` used to answer `EBADF`
  for a full table, it answers `EMFILE` since #774). The per-uid quota is
  `4 * fd_max`. A table slot is 32 bytes and the table doubles as it grows: 52 bytes of
  heap per open descriptor measured at 5000 (`bytes_per_descriptor` in
  `linux_fd_table_copies_skip_the_closed_tail`), allocated when a descriptor
  is first used, so a high ceiling costs nothing until used.
  Each thread of a process keeps its own mirror of the table
  (`task/fdshare.rs`), so a process with 1000 threads and N open descriptors
  holds 1000 x N slots; a thread's copy is only as long as the highest
  descriptor still open.

## Task slots and kernel stacks

`task::MAX_TASKS` was 256 with the kernel stacks in a static array: 12 MiB of
`.bss` on every boot, and 48 MiB at 1024 slots. Now the table is 1024 and the
stacks come from the heap when a slot is claimed (`task/kstack.rs`), so:

| | before (256 static stacks) | after (1024 slots, heap stacks) |
|---|---|---|
| Static kernel image, `.data` + `.bss` (debug build) | ~13.8 MB (12 MB of it stacks) | ~2.5 MB (the per-slot tables of 1024 slots included) |
| Cost of an idle slot | 48 KiB, always | nothing |
| Cost of a task | its slot | 49288 bytes of heap |
| Slots in use | 256 | `limit.task_max`: one per MiB of RAM, 256 .. 1024 |
| Fabric stats / system stats / task list | 16 / 20 / 22 KiB | 64 / 80 / 90 KiB (ABI versions 7 / 7 / 4) |

The snapshot blocks live on the heap (a kernel stack is 48 KiB), and
`signal::sweep` returns its terminations in batches of 128 rather than one
slot per task. A refused stack is `ENOMEM` from `clone`/`fork`. `RLIMIT_NPROC`
reports `task_max`. Tests: `task_kstack_lives_and_dies_with_its_task`,
`task_threads_fill_the_ceiling`, `task_threads_stop_before_the_heap_runs_out`,
`task_thread_ceiling_soak` (1000 generations of a fan-out to the ceiling:
no slot, stack, frame or heap byte leaks).

## Descriptor limits (`RLIMIT_NOFILE`)

`fd_max` is the hard limit and the default soft limit: **8192** (it was 1024;
Chromium raises its soft limit to 8192). The cost of raising it is heap only,
and only for descriptors really opened. A process can use `setrlimit` to lower
its soft limit, raise it back up to its hard limit and lower the hard limit
(never above `fd_max`); the limit applies to every thread of the process and is
inherited by `fork`, `execve` and `clone`. A table past a lowered soft limit
refuses `open`, `dup`, `dup2` and `F_DUPFD` (`EMFILE`/`EBADF`/`EINVAL`), and
descriptors already open stay open. Tests: `linux_nofile_*`,
`linux_fd_table_*`, `bigapp`.

## Hitting a quota

`mmap` of counted memory over `quota_user_memory` is `ENOMEM`, unmapping or
exiting returns every byte, and a refusal changes nothing (no charge, no
frames): tests
`linux_mmap_quota_exhaustion_is_enomem`, `..._soak` (2000 rounds) and
`..._returns_when_the_process_exits`.

**Reservations are charged by resident pages.** An anonymous private mapping
made `PROT_NONE` or `MAP_NORESERVE` commits nothing, so `mmap` charges nothing
for it; each page is charged to the uid when it is first touched
(`mem::demand_resolve`) and given back when it is unmapped, dropped by
`madvise(MADV_DONTNEED)`, or left behind by a shrinking `mremap`. The mapping
stays a reservation through `mprotect`. Everything else (read-write memory
without `MAP_NORESERVE`, `brk`, file and shared mappings, an ELF's segments)
is still charged by length, with `ENOMEM` at the call. A touch the quota
cannot cover cannot fail with `ENOMEM`, so it is answered like a machine with
no frame left: the process is killed (`oom: killed ...`, then `oom: uid N is
at its user-memory quota ...`), or a syscall that touched the page is refused
(`EFAULT` from the `user_ptr::try_*` copies, `ENOMEM` from `copy_in`/`copy_out`).
A runtime that probes how much it may have by watching an `mmap(MAP_NORESERVE)`
fail therefore never sees it fail: it is killed at the touch, as on Linux with
overcommit. This is what lets a JavaScript engine reserve a 1 GiB code pool and
an 8 GiB heap in each of several processes without the sum of the reservations
exhausting one uid.

Two details. The page-table frames a touch has to create are charged with the
page (a page per 2 MiB of a sparse touch pattern would otherwise pin RAM for
free) and stay charged until the address space is freed, as the tables do.
And `fork` charges the child nothing, as it always did: a child's inherited
pages are the parent's charge, so a child cannot be charged for, or give back,
what it did not touch. Tests: `linux_mmap_reservation_*` (charge by page and
by table, the refused touch at the quota and through a syscall, `mremap` moves
and shrinks, a span of reservation and counted memory, a 3000-round soak).

## Running out of memory

The quota counts reservations (apart from the ones above), not frames, so a
program can reserve more than the machine has and find out only when it touches
the last frame. Three rules keep that honest (issue #823):

* **The page fault never answers a shortage with `SIGSEGV`.** When the
  demand-zero or copy-on-write path finds the frame allocator empty, the
  faulting process is ended with `SIGKILL` (`mem::oom`, status 137, which
  `wait4` reports as the signal) and the kernel prints
  `oom: killed <comm> (<uid>): <n> MiB mapped, <n> frames`. Every thread of the
  process goes, the frames come back with its address space, and other
  processes are untouched. A bad address (no VMA, `PROT_NONE`, a store to a
  read-only page) is still `SIGSEGV`. Only a user-mode fault is killed this
  way; a kernel-mode fault that cannot get a frame keeps the old diagnostic
  halt, and the syscall copy paths (`copy_to_user` and friends) answer
  `ENOMEM` (not `EFAULT`: the pointer was fine). Two limits: the victim is
  always the process that faulted (not the largest), and its frames come back
  when its parent reaps it (`wait4`), not at the kill, so a process that
  faults in that window while the allocator is still empty is killed too.
* **A request that could never fit is refused up front** (Linux's heuristic
  overcommit, `process::linux::overcommit`): `mmap`, `brk` and growing
  `mremap` of private anonymous memory answer `ENOMEM` when the one request
  is larger than every frame the allocator owns (`FrameStats::total`), so
  `malloc` returns null and a JavaScript engine's "allocation failed" path
  runs. It is one request against the machine's total, not against what is free
  now or what the process already holds: a big sparse reservation that fits
  must work. `PROT_NONE` ranges and `MAP_NORESERVE` mappings commit nothing and
  are never refused here (the quota counts their resident pages, above). Shared mappings
  keep their stricter eager check (`fits_in_memory`, against the free frames).
* **The default quota never exceeds what the user can be given**: `3r/4`, at
  most RAM less the initial heap and the DMA pool, so a 256 MiB guest gets
  224 MiB instead of all of its RAM. A quota set in `lazyos.cfg` is kept
  whatever it is (the WebKit profile asks for 8G on a 4 GiB guest, see below), but
  one above that ceiling prints `limits: quota_user_memory=... is more than the
  ... MiB of RAM the user can be given; a process that touches more than the
  frames is killed (oom)`.

Tests: `mem_oom_kill_is_sigkill_and_returns_frames`, `mem_oom_refusals_stay_segv`,
`mem_soak_oom_kill_generations` (300 generations at the edge of the
allocator, no leak), `linux_overcommit_*`, `limits_quota_default_stays_below_ram`.
The `bigapp` row with the quota past RAM (`python tools/abi/run.py --only bigapp
--at 40 --memory 1G --limit quota_user_memory=2G --expect-oom`) reads
"killed by SIGKILL (oom)".

## The "big app" profile

For one browser-class program (dozens of threads, a few hundred MB,
thousands of descriptors):

```bash
python tools/run_demo.py --desktop --big-app
# which is --memory 4G --limit quota_user_memory=3G --limit fd_max=8192 --limit task_max=1024
```

`--big-app` takes 4G of guest RAM unless `--memory` says otherwise and each
limit unless `--limit` already sets it. At 4 GiB the defaults already give
exactly these numbers (the table above), so the profile is "4 GiB of RAM"; the
explicit limits keep it true on a branch whose defaults move. Measured with
the flags above: 1022 threads, 3064 MiB, 8192 descriptors. Beyond a browser:
`--limit fd_max=65536` (a process with 65536 open descriptors holds ~2 MB of
table), `--limit quota_user_memory=` up to what `--memory` leaves. The GUI
launcher has no checkbox for it: type the memory in the Machine settings and
`quota_user_memory=3G fd_max=8192 task_max=1024` in the Advanced tab's
"Kernel limits".

**The WebKit browser** (`run_demo.py --webkit`, docs/webkit-port-plan.md W5) adds `quota_user_memory=8G`
(`lazygui.limits.WEBKIT_LIMITS`; `--limit` still wins). The quota used to charge the *length* of every
`mmap`, and the three WebKit processes (the UI process, one WebProcess, the NetworkProcess) took what it let
them: 2989 MiB of the profile's 3072 after the first page and 3059 after seven, 6957 and 7027 MiB of 8192 at
8G, while 3405 of 4019 MiB of RAM stayed free (each process's JIT pool and structure heap are
`MAP_NORESERVE` reservations, and the structure heap halves its request until it fits). A second WebProcess
(a page with `Cross-Origin-Opener-Policy`, such as www.iana.org, starts one whatever
`ProcessSwapsOnNavigation` says) got a 64 MiB structure heap, ran out and crashed in `fastMalloc`. Reservations
are charged by resident page now (above), so a process costs what it touches and the headroom stays.

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
| `task::MAX_TASKS` | 1024 (pid == slot) | The task table, the per-slot registries (credentials, handle tables, FPU areas, argument blocks) are static arrays, and the task/sysinfo/fabric snapshot ABIs carry one row per slot (`libs/lazyos-sys/src/sysinfo/`, `msg/fabric.rs` and `user/src/task_snapshot.rs` mirror it); changing it bumps their versions. The kernel stacks are not part of it: they come from the heap (`limit.task_max` is the knob) |
| User address space | ~127.5 TiB private window, then a 512 GiB shared-buffer window | PML4 entries 0..254 / 255 (`mem::layout`); far beyond any RAM |
| Kernel heap span | 512 GiB | One PML4 entry, so heap growth is visible in every address space |
| Usable memory regions | 128 after merging | `mem::regions`; the smallest are dropped (and logged) past that |
| Per-process live shared buffers | 64 | `ipc::shared::MAX_BUFFERS_PER_PROCESS`; the bytes are configurable |
| Linux `mmap` area | 96 TiB (`process::layout`) | Layout, not memory: quota and RAM bound use |

**Status.** Working; covered by `limits_suite`, `heap_suite` (growth, ceiling,
soak), `linux_suite::fd_table` / `nofile_limit` / `mmap_quota`,
`task_suite::slots`, `loader_suite`, `mem_suite::layout`, the `bigapp` ABI row
and the build-side `limits_tests`.
