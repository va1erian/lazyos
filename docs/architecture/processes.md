# Processes, supervision & Linux ABI

**What it is.** Process identity and the process tree, the native supervision
syscalls (`spawn`/`wait`/`clock`/`args`/`creds`), the ELF loaders, and the Linux
syscall shim.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/process.rs` | Tree, groups, sessions, `finish`, `reap_child` |
| `kernel/src/process/mod.rs` | Native gate dispatch, ELF loader, syscalls 6-11 |
| `kernel/src/process/linux.rs` | Linux ELF loader + syscall dispatch, futex, clone |
| `kernel/src/ipc/pipe.rs` | Pipes (`pipe`/`pipe2`) and `AF_UNIX` socket pairs |
| `kernel/src/arch/linux.rs` | `syscall`/`sysret` entry (see [arch.md](arch.md)) |
| `kernel/src/task/signal.rs` | `SIGCHLD`, group termination (see [wait-signals.md](wait-signals.md)) |

**Identity model** (`task/process.rs`, issue #59)

- `pid == scheduler slot`; slots are recycled. `pgid` and `sid` hold the pid of
  the leader. The tree is derived by scanning for `parent == slot` (cheap at
  `MAX_TASKS = 16`) rather than stored as child lists.
- `finish` is the single death path: marks `Done`, records `exit_status`,
  re-parents children to the kernel task (`KERNEL_TASK`, pid 0), posts
  `SIGCHLD` and notifies `CHILD_EXIT`.
- `setpgid`/`setsid`/`getpgid`/`getsid` follow Linux's permission rules
  (same session, no moving a session leader, `EPERM` on cross-session), with
  failures mapped through `GroupError`. `process_list()` is the introspection
  API; `kill_group` is the blunt group-termination primitive kept for tests,
  while signals drive per-process termination.

**Native syscalls** (`process/mod.rs`)

| Nr | Signature | Purpose |
|---|---|---|
| 0 | `exit(code)` | finish current task; status visible to the supervisor |
| 1-4 | `write`, `read_char`, `read_file`, `sbrk` | demo surface |
| 5 | `messenger(op, args, result)` | fabric (see [ipc-fabric.md](ipc-fabric.md)) |
| 6 | `spawn("PATH.ELF [args]")` | child of caller; stores args per slot |
| 7 | `wait(deadline)` | reap a child; returns packed pid/status, or `-1` on timeout |
| 8 | `clock()` | PIT ticks (100 Hz) for backoff and polls |
| 9 | `args(buf, len)` | copy the manifest argument string |
| 10 | `creds(op, a1, a2)` | audited credential gate (see [ipc-security.md](ipc-security.md)) |
| 11 | `quota(buf)` | per-uid usage/limit block |
| 12 | `display(op, ...)` | display grant (see [display.md](display.md)) |

- `spawn` reads the ELF from the FAT image, leaks one interned `&'static str`
  per distinct service name, and resets the slot's credentials before the child
  can run; `SERVICE_ARGS` is keyed by slot and cleared on reuse. `load_image`
  maps `PT_LOAD` segments (prot from `PF_W`/`PF_X`, `File` VMA) and the stack
  eagerly at `USER_HEAP_BASE = 0x60_0000` / `USER_STACK_TOP = 0x80_0000`
  (`USER_STACK_SIZE = 0x2_0000`).

**Linux shim** (`process/linux.rs`, issues #55-#60; plan: [linux-abi-plan.md](../linux-abi-plan.md))

- `load` builds a Linux stack (argv/envp/auxv; `AT_CLKTCK = 100`) and returns
  `(entry, stack_top)`; `spawn_linux` registers the `brk`/`mmap` bumps.
- `linux_dispatch` implements a growing subset: file I/O (`read`, `write`,
  `openat`, `close`, `stat`/`fstat`, `getdents64`, `readv`/`writev`, `lseek`,
  `mkdir`/`mkdirat`, `rmdir`, `rename`/`renameat`, `unlink`/`unlinkat` with
  `AT_REMOVEDIR`), memory (`mmap`, `mprotect`, `munmap`, `brk`), signals
  (`rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `sigaltstack`), process
  (`clone`, `fork`, `execve`, `exit`, `exit_group`, `wait4`, `kill`, `tkill`,
  `tgkill`, `set_tid_address`, `futex`), ids/groups (`getpid`/`gettid`,
  `getppid`, `setpgid`, `setsid`, `getpgid`, `getsid`), pipes/sockets (`pipe`,
  `pipe2`, `socketpair`, `sendto`/`recvfrom`, `poll`), and time/misc
  (`nanosleep`, `clock_gettime`, `gettimeofday`, `getrandom`, `uname`, `access`,
  `umask`, `arch_prctl`, `sched_getaffinity`). Everything else logs
  `ENOSYS <nr> <name>`.
- File I/O runs against the ABI's own mount table, whose root is the copy-up
  overlay from [filesystem.md](filesystem.md), so `O_CREAT`, `O_TRUNC`,
  `O_APPEND`, `O_EXCL`, and `O_DIRECTORY` open, `mkdir`/`rename`/`unlink`/
  `rmdir`, and descriptor writes all succeed over the read-only FAT boot
  volume. Relative `*at` calls join a real directory descriptor's recorded
  path (which is how `std`'s `remove_dir_all` walk works); descriptors snapshot
  file bytes at open and `write` patches the snapshot after updating the
  backing file.
- Honored `clone` flags: `CLONE_VM`, `CLONE_SETTLS`, `CLONE_PARENT_SETTID`,
  `CLONE_CHILD_CLEARTID`. `CLONE_VM` with `CLONE_THREAD` is a pthread; without
  it, musl's `posix_spawn` vfork child (a copy-on-write process with the
  parent's descriptor table). Futex words get one `WaitQueue` per address.
- Pipes are bounded 64 KiB byte rings with reader/writer refcounts, blocking
  waits on the task wait queues, EOF when the last writer closes and `-EPIPE`
  when the last reader closes (no SIGPIPE; see the module docs). `F_GETFL`/
  `F_SETFL` carry `O_NONBLOCK`, `F_GETFD`/`F_SETFD` carry `FD_CLOEXEC`, and
  `execve` closes marked descriptors while `fork` inherits them.
- `execve` replaces the image, resets the signal table, and tears down the old
  PML4 when it has no other users. The ABI bench injects `INIT.ELF` via
  `LAZYOS_INIT`; `BUSYBOX` runs BusyBox `sh` on the shim, and results are
  generated into `docs/compat/` (git-ignored) by `tools/abi/run.py`.

**Status.** Working: BusyBox `sh`, static musl fixtures including
`std::process` with piped stdio (matrix published by CI), native supervision
loop (`init`). Gaps tracked in the Linux ABI plan: `SOCK_SEQPACKET` message
framing, `poll` edge cases, full `SA_RESTART`, shared file tables.
