# Plan: Linux x86_64 ABI shim on LazyOS

> **Status (2026-09-28): implemented.** Phases L0-L4 below all landed (issues
> #32-#41, #120, #133-#136, #155, #179), and the five success criteria hold:
> the 13 fixtures in `tools/abi/fixtures` pass in CI and BusyBox `sh` runs. The
> shim now also covers `mremap`, epoll/eventfd, `AF_UNIX` stream and seqpacket
> sockets and a writable root: since filesystem F2-F5 (#506, #525) the ABI
root in the configured layout is the ext2 OS volume itself, with no overlay,
and the copy-up overlay and `/data` remain only in the `legacy` fallback
layout (`kernel/src/fs/mounts.rs`). The sections below are kept as the
> design record; the current state of the code is described in
> [`architecture/processes.md`](architecture/processes.md),
> [`architecture/arch.md`](architecture/arch.md) and
> [`architecture/virtual-memory.md`](architecture/virtual-memory.md). One
> design point changed during implementation: the entry stub does **not** use
> `swapgs`/per-CPU GS state; it switches to the task's kernel stack through a
> global updated on every context switch (see `arch.md`, "Linux entry
> decisions"). Remaining gaps are listed in the processes page.

> **Round 3 (2026-10-03): real CLI software.** Unmodified upstream programs
> now run: `dash` 0.5.12, `lua` 5.4.7, `sqlite3` 3.46.1, `jq` 1.7.1 and
> ripgrep 14.1.1, built from pinned, hash-checked sources by
> `tools/linuxapps/build.py` (zig for C, the musl target for Rust) and placed
> in `/system/bin` by `LAZYOS_LINUXAPPS=1` (`run_demo.py --linuxapps`, or the
> launcher's checkbox). The bench runs each as a row (`dash`, `lua`, ...), plus
> a `compat` fixture for the calls below. What changed in the shim:
>
> * **futex**: `REQUEUE`, `CMP_REQUEUE`, `WAKE_OP` and the bitset ops are real;
>   wait timeouts are honoured (relative for `WAIT`, absolute on
>   `CLOCK_MONOTONIC`/`CLOCK_REALTIME` for `WAIT_BITSET`); waiters are keyed by
>   address space and address; the PI family and `FUTEX_FD` answer `ENOSYS`
>   instead of a silent 0 (`process/linux/futex.rs`, `futex_queue.rs`).
> * **processes**: `wait4` honours its pid argument (one child, any, own group,
>   `-pgid`) and reports `WIFSIGNALED`/`WCOREDUMP`; `waitid`; `vfork` (as a
>   copy-on-write fork); `clone` refuses unknown flags, does process-style
>   clones, writes `pid_t`-sized tids, and implements `CLONE_FILES`/`CLONE_FS`
>   (a thread used to get a *fresh* descriptor table); `getpid` of a thread is
>   its group leader; `/proc/self/exe` names the running program.
> * **signals**: `SA_RESTART` restarts interrupted transfers, waits and lock
>   calls; `pause`; `ppoll`/`pselect6`/`epoll_pwait` install their mask like
>   `sigsuspend`.
> * **terminals**: a real line discipline (`kernel/src/tty/`) with
>   `TCGETS`/`TCSETS*`, canonical editing and echo, `VMIN`/`VTIME`, `ISIG`
>   to the foreground group (`TIOCSPGRP`), window size; pseudo-terminals
>   (`/dev/ptmx`, `/dev/pts/N`, `TIOCGPTN`/`TIOCSPTLCK`/`TIOCSCTTY`), which the
>   desktop Terminal now hosts its shell on. Pipes are no longer reported as
>   terminals (`ioctl` and `fstat` used to lie for descriptors 0-2).
> * **new syscalls**: `select`, `pselect6`, `ppoll`, `sched_yield`,
>   `sched_getparam`/`getscheduler`/`get_priority_*`, `flock` and `fcntl`
>   record locks (`F_GETLK`/`F_SETLK`/`F_SETLKW`, OFD forms), `sendmsg`/
>   `recvmsg` (no ancillary data), `MSG_PEEK`/`MSG_DONTWAIT`/`MSG_WAITALL`,
>   `getrlimit`/`setrlimit`/`prlimit64` (the real limits; only no-op changes
>   accepted), `sysinfo`, `times`, `getrusage`, `getgroups`/`setgroups`,
>   `readlinkat`, `faccessat`/`faccessat2`, `dup3`, `renameat2`
>   (`RENAME_NOREPLACE`), `epoll_create`, `eventfd`, `getcpu`; `link`/
>   `symlink`/`linkat`/`symlinkat` answer `EPERM` (no filesystem stores links).
> * **no more silent stubs**: `madvise` (`DONTNEED` really zero-fills, unknown
>   advice is `EINVAL`), `prctl` (names, dumpable, no-new-privs; the rest
>   `EINVAL`), `set_robust_list` (walked at thread exit: owner-died + wake),
>   `reboot` (`EPERM`: only `init` stops the machine).
> * **files**: fabricated `/etc` (`passwd` and `group` from the account file
>   without secrets, `hosts`, `resolv.conf` naming QEMU's `10.0.2.3`,
>   `hostname`, `os-release`, `shells`), more `/proc` (`cpuinfo`, `meminfo`,
>   `uptime`, `loadavg`, `stat`, `version`, `filesystems`, `self/stat`,
>   `self/status`, `self/comm`, `self/fd/N` links), `/dev/zero`/`/dev/full`
>   behave, `/dev/random`/`/dev/urandom` exist; `readlink` of a plain file is
>   `EINVAL` (what `realpath` needs).
>
> Still missing, by how much software it blocks: dynamic linking (static-PIE
> and static binaries only); a per-thread signal mask and per-thread signal
> targeting (both still per process); `timerfd`/`signalfd`/`inotify`/
> `memfd_create`; `alarm`/`setitimer`; descriptor passing (`SCM_RIGHTS`);
> `SIGTTIN`/`SIGTTOU` job-control stops; symlinks and hard links in the
> filesystems; record locks are not dropped when *one* descriptor of the file
> closes (only at unlock or exit). The kernel test suite for all of this is
> `kernel/src/tests/linux_compat_suite/`.

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
   `/system/bin/hello`; the native shell was retired in issue #254 in favour of BusyBox
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

Status (#136, historical; now the `legacy` layout only): the ABI root was a copy-up overlay over the read-only FAT volume
(upper layer in ramfs, whiteouts for deletes), so `O_CREAT`/`mkdir`/`rename`/
`unlink`/`rmdir` and descriptor writes work without a writable FAT driver; see
[architecture/filesystem.md](architecture/filesystem.md).

Status (#334, historical): files under `/data` (the ext2 data volume) became VFS-backed
descriptors rather than snapshots, with `pread64`/`pwrite64`/`truncate`/
`ftruncate`/`fsync`/`fdatasync`/`syncfs`/`sync`/`statfs`/`fstatfs`, POSIX
unlink-while-open, and the two-boot `persist` fixture proving a file survives a
reboot. `chmod`/`chown`/`utimensat` and `link`/`symlink` remain
`ENOSYS` (the VFS trait has no attribute setter or link nodes yet). Today every
ext2 mount is VFS-backed this way (`fs::abi_persistent`), which in the
configured layout includes `/` and `/home`; `/data` exists only in the legacy
layout.

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
  added to the disk image in `build.rs` like `/system/bin/hello`.
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
