# Running Rust's `std` on LazyOS — what it takes

_Assessment written 2026-09-27. Goal: `std`-linked Rust programs (not
`#![no_std]`) running on LazyOS._

> **Outcome:** Route B was chosen and implemented; see
> [`linux-abi-plan.md`](linux-abi-plan.md) and
> [`architecture/processes.md`](architecture/processes.md). The "Where LazyOS
> stands today" section below describes the kernel *before* that work and is
> kept only as the record of the decision; every item in its "missing" list has
> since landed (`mmap`/`brk`, TLS via `arch_prctl`, futex with blocked tasks,
> `clone` threads, clocks, `getrandom`, argv/envp).

## The core fact

`std` is **not** portable to an arbitrary OS by linking it. It is ported
*per target*: `library/std/src/sys/pal/<os>/` contains the platform code that
implements the OS-facing half of `std`. Existing ports include `windows`,
`unix`, `wasi`, `hermit`, `xous`, `uefi`, `sgx`, `zkvm`, … and there is an
`unsupported` scaffold (just `common.rs` + `mod.rs`). So "run std" means either
**writing a new PAL for LazyOS**, or **pretending to be an OS that already has
one** (usually Linux).

## What `std` needs from the OS

Roughly, by `std` area:

| `std` area | OS service needed |
|---|---|
| `alloc` (`Vec`, `String`, `Box`) | a global allocator + growable memory (`brk`/`mmap`) |
| `io::stdout/stderr` | `write` to fd 1/2; `io::stdin` → `read` fd 0 |
| `fs` | `open`/`read`/`write`/`close`/`fstat`/`lseek` (and `unlink`/`mkdir` for full API) |
| `thread` | thread creation (`clone`-like), thread exit/join, TLS, per-thread stacks |
| `sync` (`Mutex`, `Once`, `Condvar`) | **futex** (wait/wake) or an equivalent |
| `thread::park` / `unpark` | futex / a park primitive |
| `time` (`Instant`, `SystemTime`) | a monotonic clock + wall clock (`clock_gettime`) |
| `collections::HashMap` | randomness for `RandomState` (a `getrandom`-like source) |
| `env` / `args` | argv/envp passed by the loader |
| `process` | `fork`/`exec`/`wait` (optional at first) |
| `net` | sockets (optional) |
| `panic` | unwinder (`_Unwind_*`) **or** build with `panic = "abort"` |
| runtime init | `lang_start`, `main` symbol, thread-local destructors, `at_exit` |
| stack overflow guard | a SIGSEGV-equivalent + `sigaltstack` (nice-to-have) |

Two things bite hardest and are easy to underestimate:

- **TLS (thread-local storage).** `std` uses `#[thread_local]` extensively
  (`std::thread::current`, errno, panic state). This requires handling the
  ELF `PT_TLS` segment and setting the thread pointer (`%fs` on x86_64) per
  thread via something like Linux's `arch_prctl(ARCH_SET_FS)`.
- **futex.** `std::sync::Mutex`, `Once`, `Condvar` and thread parking all go
  through a futex on Linux-family ports. You can provide a trivial
  futex-wait/wake (a single-CPU "spin + yield" is enough), but it must exist
  for `std` to link and run.

## Two routes

### Route A — a real `std` port for LazyOS (the "native" way)

1. Add a target spec, e.g. `x86_64-unknown-lazyos.json` (`"os": "lazyos"`).
2. Create `library/std/src/sys/pal/lazyos/` implementing the PAL modules the
   target `os` dispatches to: `stdio`, `fs`, `os`, `thread`, `sync`, `time`,
   `random`, `process`, `args`, `env`, `memmap`, `pipe`, `abort`, plus a
   `thread_parking` and `stack_overflow` analogous to the `unix` PAL.
3. Wire it into `std`'s `cfg` dispatch (`sys/pal/mod.rs`, `sys/mod.rs`).
4. Build `std` for that target with `-Z build-std` (nightly).
5. Implement the corresponding syscalls in the kernel.

Consequences: you are modifying `std` itself, so you either **fork the Rust
toolchain** or keep a vendored `library/std` (with `-Z build-std`) and keep it
in sync. This is how Hermit/Xous/WASI have `std`. It is the "correct" endpoint
but a long road.

### Route B — a Linux ABI shim, reuse the existing `std` (the shortcut)

Implement enough of the **Linux x86_64 syscall ABI** that prebuilt
`x86_64-unknown-linux-musl` static binaries run. `std`/musl already target
Linux, so you don't touch Rust's sources at all: you provide `syscall`
dispatch for the numbers they use, with the exact Linux struct layouts.

Minimum practical syscall set for "hello world + HashMap + files + threads":

- memory: `brk`, `mmap`, `mprotect`, `munmap`
- io: `read`, `write`, `openat`, `close`, `lseek`, `fstat`/`newfstatat`, `readlink`
- threads: `clone`, `exit`, `exit_group`, `futex`, `set_tid_address`,
  `set_robust_list`, `sched_getaffinity`, `rt_sigaction`, `rt_sigprocmask`,
  `arch_prctl` (SET_FS), `getrandom`
- time: `clock_gettime`, `nanosleep`, `gettimeofday`
- misc: `uname`, `getpid`, `getcwd`, `prctl`, `sysinfo`

The catch: these are a *lot* of small, precisely-defined interfaces, and the
struct layouts must match exactly (e.g. `stat`, `iovec`, `sockaddr`). But it's
a fixed, documented surface — and once it works, the whole musl/std ecosystem
runs with zero Rust-side maintenance.

## Where LazyOS stands today

Already in place (from the roadmap work):

- ring 3 + per-task address spaces, a syscall gate, preemptive scheduling;
- a global frame allocator and a user heap (`sbrk`) — the allocator half;
- a FAT12/16 filesystem, PS/2 input, a timer.

Missing for either route:

- `mmap`/`brk` semantics distinct from our bump `sbrk` (and guard pages);
- **TLS setup** (ELF `PT_TLS` + `%fs` per thread) in the loader/scheduler;
- **futex** + a real blocked-task state (we currently spin, relying on the
  timer to preempt);
- per-thread kernel/user stacks and `clone`-style thread creation;
- a monotonic/wall clock syscall; a randomness source;
- argv/envp passing to new processes;
- either a forked `std` (Route A) or a Linux syscall layer (Route B).

## Effort

- **Route B to "runs a real musl static binary that prints and allocates"**:
  no Rust changes; implement ~10 syscalls (`mmap`, but ideally `brk`, `write`,
  `exit`, `arch_prctl`, `getrandom`, `clock_gettime`, `futex`, `clone` for
  threads). Weeks, mostly debugging ABI details.
- **Route B to "threads + HashMap + std::fs + time"**: the full list above;
  weeks more.
- **Route A (native PAL)**: weeks to get a minimal PAL booting, months to be
  broadly correct, plus ongoing sync with `std`.

Recommendation: if the goal is "run ordinary Rust programs", **Route B** gives
the most capability per unit effort and requires touching no Rust sources. If
the goal is a *first-class* Rust platform with its own target triple, **Route A**
is the destination — and Hermit/Xous are good templates to copy structure from.

## Smallest credible first step

Whichever route: implement **`mmap`/`brk` with a blocked-task state and TLS**.
Those three unlock real runtimes far more than any other single piece; a
`std`-linked binary cannot even reach `main` without a thread pointer.
