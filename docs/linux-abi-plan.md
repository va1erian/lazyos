# Plan: Linux x86_64 ABI shim on LazyOS

> **Status (2026-09-28): implemented.** Phases L0-L4 below all landed (issues
> #32-#41, #120, #133-#136, #155, #179), and the five success criteria hold:
> the 13 fixtures in `tools/abi/fixtures` pass in CI and BusyBox `sh` runs. The
> shim now also covers `mremap`, epoll/eventfd, `AF_UNIX` stream and seqpacket
> sockets and a copy-up writable root. The sections below are kept as the
> design record; the current state of the code is described in
> [`architecture/processes.md`](architecture/processes.md),
> [`architecture/arch.md`](architecture/arch.md) and
> [`architecture/virtual-memory.md`](architecture/virtual-memory.md). One
> design point changed during implementation: the entry stub does **not** use
> `swapgs`/per-CPU GS state; it switches to the task's kernel stack through a
> global updated on every context switch (see `arch.md`, "Linux entry
> decisions"). Remaining gaps are listed in the processes page.

**Goal:** run ordinary, prebuilt **`x86_64-unknown-linux-musl` static binaries**
(including Rust programs that link `std`) on LazyOS, by implementing enough of
the Linux x86_64 ABI that musl's startup and `std`'s runtime are satisfied — no
changes to Rust's sources. See `docs/rust-std.md` (Route B) for why this route
was chosen over a native `std` port.

**Success criteria (in order):**
1. A static musl binary prints and exits.
2. `Vec`/`String`/`Box`/`HashMap` and `format!` work (allocator + randomness).
3. `std::fs` reads a file from the FAT volume; `Instant`/`SystemTime` work.
4. `std::thread::spawn` + `Mutex`/`Condvar` work (clone + futex).
5. (Stretch) `std::process::Command` (fork/execve/wait4).

## Why this is not just "more syscalls"

Two things are structural, not incremental:

- **Entry uses `syscall`/`sysret`, not `int 0x80`.** x86_64 musl issues
  `syscall`; args in `rdi,rsi,rdx,r10,r8,r9`, number in `rax`, result in `rax`.
  This needs MSRs (`IA32_LSTAR`, `IA32_STAR`, `IA32_FMASK`) and a kernel-stack
  switch on entry (`swapgs` + per-CPU kernel stack), since `syscall` does not
  switch stacks.
- **GDT selector layout must match `syscall`/`sysret`.** `SYSCALL` loads
  `CS = STAR[47:32]`, `SS = CS+8`; `SYSRET` loads `CS = STAR[63:48]+16`,
  `SS = STAR[63:48]+8`. With `kcode=0x08`, `kdata=0x10`, this forces **user
  data at 0x18 and user code at 0x20** (data *before* code) — the opposite of
  LazyOS's current order. The GDT must be reordered for the Linux path (our
  native `int 0x80` path can keep its own layout, or be moved onto the same one).

## Architectural changes to LazyOS

1. **Real virtual memory.** Replace the fixed code/heap/stack regions with an
   address-space object that supports *anonymous and file* mappings plus
   `brk`, `mmap`, `mprotect`, `munmap`, `mremap`. This is the biggest piece.
2. **Blocked task state.** `futex`/`nanosleep`/`clone`/`wait4` need tasks that
   sleep and are woken, not the current "spin + timer preemption".
3. **Process/thread model.** Threads share an address space (`CLONE_VM`), have
   their own stacks and TLS; processes are separate address spaces.
4. **Linux process bootstrap.** Build the Linux start stack (argc/argv/envp/auxv)
   and load a static ELF (segments + `PT_TLS`), passing `AT_PHDR`/`AT_ENTRY`.
   Static musl initialises TLS itself from `PT_TLS` + `mmap` + `arch_prctl`.
5. **Two program kinds.** Keep the native LazyOS ABI (used by the services and
   `HELLO.ELF`; the native shell was retired in issue #254 in favour of BusyBox
   `sh`, which runs those programs through `execve`, see
   `docs/architecture/processes.md`); add a
   "linux" kind with the `syscall` gate and Linux syscall table. The
   multiplexer can host one of each.

## Phases

### L0 — Boot a static "hello world" (the proving ground)
Deliverables: `syscall`/`sysret` gate (MSR setup, per-task kernel stack via
`swapgs`); Linux ELF loader (reuse `process::load_image`, add start stack);
minimal VM (anonymous `mmap`, `brk`, `munmap`, `mprotect`); syscall dispatch.

Syscalls: `write`(1) `exit`(60) `exit_group`(231) `brk`(12) `mmap`(9)
`munmap`(11) `mprotect`(10) `arch_prctl`(158) `set_tid_address`(218)
`set_robust_list`(273) `getrandom`(318) `clock_gettime`(228)
`rt_sigaction`(13) `rt_sigprocmask`(14) `sigaltstack`(131) `sched_getaffinity`(204)
`getpid`(39) `gettid`(186) `uname`(63) `getcwd`(79) `rseq`(334 → `ENOSYS`).

Acceptance: a musl `println!("hello")` binary prints over the serial/console.

### L1 — `std` core: alloc, HashMap, formatting
Confirm `Vec`/`String`/`format!`/`Box` allocate via `mmap`/`brk`, and that
`getrandom` seeds `HashMap`. Syscalls: none new beyond L0 (mostly validating
the memory subsystem under musl's allocator).

### L2 — Filesystem and time
Syscalls: `openat`(257) `close`(3) `read`(0) `write`(1) `lseek`(8)
`newfstatat`(262)/`fstat`(5) `readv`(19)/`writev`(20) `getdents64`(217)
`readlink`(89) `access`(21) `fcntl`(72) `ioctl`(16) `dup`(32)/`dup2`(33)
`getcwd`(79) `clock_getres`(229) `nanosleep`(35). Map fds 0/1/2 to the task's
terminal; map `open`/`read`/`getdents` onto the FAT reader (read-only first).

Status (#136): the ABI root is a copy-up overlay over the read-only FAT volume
(upper layer in ramfs, whiteouts for deletes), so `O_CREAT`/`mkdir`/`rename`/
`unlink`/`rmdir` and descriptor writes work without a writable FAT driver; see
[architecture/filesystem.md](architecture/filesystem.md).

### L3 — Threads and synchronization
Syscalls: `clone`(56) `futex`(202, at least `WAIT`/`WAKE`/`REQUEUE`/`CMP_REQUEUE`)
`sched_yield`(24) `set_tid_address`(218) `set_robust_list`(273) `madvise`(28)
`mremap`(25). Add per-thread stacks and TLS (`CLONE_SETTLS` →
`CLONE_CHILD_CLEARTID`). Accept `Mutex`, `Condvar`, `thread::spawn`.

### L4 — Processes and signals (stretch)
`fork`(57)/`clone` with process flags, `execve`(59), `wait4`(61), `kill`(62),
real `rt_sigaction` + `rt_sigreturn`(15) delivery, `pipe2`(293). Enables
`std::process::Command` and shell pipelines.

## Bootstrap details (the fiddly bits)

- **Start stack:** `rsp` → `argc`, `argv[]`, NULL, `envp[]`, NULL, `auxv[]`
  (pairs of u64). Strings live above. Provide at least: `AT_PHDR`, `AT_PHENT`,
  `AT_PHNUM`, `AT_PAGESZ=4096`, `AT_BASE=0`, `AT_ENTRY`, `AT_RANDOM` (16 bytes),
  `AT_HWCAP`, `AT_CLKTCK=100`, `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`, and
  `AT_SYSINFO_EHDR=0` (no vDSO; musl falls back to raw syscalls for time).
- **TLS:** no kernel TLS work is required for *static* musl — it mmaps the TLS
  block, builds the TCB, and calls `arch_prctl(ARCH_SET_FS, tp)`. We only need
  a working `arch_prctl` and correct `AT_PHDR`. (Threads later reuse this via
  `CLONE_SETTLS`.)
- **`stat`/`statx` layouts** must match x86_64 exactly, or `std::fs` breaks.
- **`ERESTARTSYS`/`EINTR`:** blocking syscalls must return restartable errors
  like the kernel does, or musl misbehaves on signals.
- **`swapgs` discipline:** user GS vs kernel GS must be swapped correctly on
  `syscall` entry and `sysret` exit.

## Testing strategy

- **Host build:** `cargo build --target x86_64-unknown-linux-musl --release`
  (musl static, no dynamic loader needed). For the fastest signal, start with a
  tiny C program (`gcc -static`) that prints, then move to Rust `std`.
- **Fixtures** live in a new `user-linux/` crate (or a `fixtures/` dir) and are
  added to the disk image in `build.rs` like `HELLO.ELF`.
- **LazyOS launcher:** reuse `process::spawn` but tag the task as Linux-ABI; run
  it in a multiplexer window.
- **Verification:** serial lines + `tools/screenshot/qemu_session.py`, as with
  every other feature; CI keeps a Linux-binary smoke test.

## Risks

| Risk | Mitigation |
|---|---|
| VM subsystem is a large refactor | Land `mmap`/`brk` with a simple VMA list first; file mappings can start read-only |
| `syscall`/`sysret` + `swapgs` bring-up is delicate | Prove with a hand-written asm test returning a constant before wiring musl |
| SIGSEGV handler at startup (stack-overflow reporting) | Store handlers and never deliver initially; document it |
| musl version drift | Pin a musl/std version in the fixture; ABI is stable and documented |
| Two ABI families in one kernel | Isolate per task kind (`Native` vs `Linux`); share VM/scheduler |

## Effort

- **L0:** the bulk (VM + `syscall` gate + bootstrap). Days–weeks.
- **L1:** small (validation).
- **L2:** medium (fd layer over FAT + stat layout).
- **L3:** medium–large (futex + clone + TLS threads).
- **L4:** large (processes/signals), optional.

## Smallest first step

Implement **`mmap`/`brk` with a blocked-task state**, then the **`syscall`
gate**, then the **start stack**, and boot a static `hello world`. Everything
else is incremental on top of that spine.
