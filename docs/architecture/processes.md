# Processes, supervision & Linux ABI

**What it is.** Process identity and the process tree, the native supervision
syscalls (`spawn`/`wait`/`clock`/`args`/`creds`), the ELF loaders, and the Linux
syscall shim.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/process.rs` | Tree, groups, sessions, `finish`, `reap_child` |
| `kernel/src/process/mod.rs` (+ `creds.rs`, `spawn.rs`, `spawnv.rs`, `argstore.rs`) | ELF loader, syscalls 7-11 and 30 (creds/quota/tasks in `creds.rs`, task-name interning and the exec gate in `spawn.rs`, `spawnv` in `spawnv.rs`, the per-task argv/envp blocks syscall 9 reads in `argstore.rs`) |
| `kernel/src/process/gate.rs` | The `int 0x80` gate: register-save stub and the syscall routing table (native dispatch) |
| `kernel/src/process/linux/` | Linux ELF loader + syscall dispatch, futex, clone (split by syscall family; see its `mod.rs` doc comment) |
| `kernel/src/ipc/pipe.rs` | Pipes (`pipe`/`pipe2`) and `AF_UNIX` socket pairs |
| `kernel/src/arch/linux.rs` | `syscall`/`sysret` entry (see [arch.md](arch.md)) |
| `kernel/src/task/signal.rs` | `SIGCHLD`, group termination (see [wait-signals.md](wait-signals.md)) |

**Identity model** (`task/process.rs`, issue #59)

- `pid == scheduler slot`; slots are recycled. `pgid` and `sid` hold the pid of
  the leader. The tree is derived by scanning for `parent == slot` (cheap at
  `MAX_TASKS = 1024`) rather than stored as child lists.
- `finish` is the single death path: marks `Done`, records `exit_status`,
  re-parents children to the kernel task (`KERNEL_TASK`, pid 0), posts
  `SIGCHLD` and notifies `CHILD_EXIT`.
- `setpgid`/`setsid`/`getpgid`/`getsid` follow Linux's permission rules
  (same session, no moving a session leader, `EPERM` on cross-session), with
  failures mapped through `GroupError`. `process_list()` is the introspection
  API; `kill_group` is the blunt group-termination primitive kept for tests,
  while signals drive per-process termination.

**ELF loading** (`process/loader.rs`, `elfhdr.rs`, `image.rs`)

- Every spawn path (`spawnv`, `execve`, the native exec, the kernel's boot
  spawns) hands the loader an `Image`: a `VfsFile` streamed through the ABI or
  native VFS, or a byte slice (embedded images, tests). Nothing reads a whole
  program into the kernel heap; the loader reads the ELF header and program
  headers, then copies each segment's file bytes in 128 KiB chunks straight
  into the new frames.
- Headers are validated before anything is mapped (little-endian ELF64,
  x86-64, `ET_EXEC` or `ET_DYN`, 56-byte program headers inside the
  file; every `PT_LOAD` inside the file, below the layout's `MMAP_BASE`, clear
  of the reserved windows, no two segments sharing a byte, the entry inside a
  segment). There is no cap on image size or segment count: the overlap check
  sorts the segments and compares neighbours.
- A program with `PT_INTERP` is dynamically linked (issue #535): the Linux
  loader maps the interpreter named there (read as stored, else the file of
  that name in `/system/lib` for `/lib`, `/usr/lib`, `/usr/local/lib` and
  `/lib64`) at `INTERP_BASE` (1 GiB below `MMAP_LIMIT`), moves a PIE program to
  `PIE_BASE` (`0x40_0000`), starts at the interpreter's entry and passes
  `AT_BASE` (the interpreter's load bias), `AT_PHDR` and `AT_ENTRY` of the
  program; musl's `ld-musl-x86_64.so.1` then maps the libraries with
  file-backed `mmap`. An interpreter that is missing is `ENOENT`, one that
  is not an ELF or has a `PT_INTERP` itself `ENOEXEC`. Static and static-PIE
  images load exactly as before.
- Pages holding file bytes are mapped eagerly; a segment's `.bss` tail past
  its last file page is an `Anon` VMA, zero-filled on first touch, so the
  eager cost is bounded by the file, not by what its headers claim.
- The Linux loader (`linux/elf.rs`) then maps the main-thread stack
  (`limit.stack_size`, 8 MiB by default, only the start frame's pages present)
  and starts `brk` on the page after the image. The layout is in
  [virtual-memory.md](virtual-memory.md).

**Native syscalls** (`process/mod.rs`)

| Nr | Signature | Purpose |
|---|---|---|
| 0 | `exit(code)` | finish current task; status visible to the supervisor |
| 1-4 | `write`, `read_char`, `read_file`, `sbrk` | demo surface |
| 5 | `messenger(op, args, result)` | fabric (see [ipc-fabric.md](ipc-fabric.md)) |
| 6 | (retired) | was the command-line `spawn("PATH [args]")`; `spawnv` (31) replaced it in fs F3 and the number is now an unknown syscall |
| 7 | `wait(deadline)` | reap a child; returns packed pid/status, or `-1` on timeout |
| 8 | `clock()` | PIT ticks (100 Hz) for backoff and polls |
| 9 | `args(buf, len, which)` | copy the caller's `argv` (`which` 0, `argv[0]` included) or `envp` (`which` 1) block of NUL-terminated strings; returns the full length, copies at most `len`; `-EINVAL` for another `which` (`process/argstore.rs`) |
| 10 | `creds(op, a1, a2)` | audited credential gate: `set`, `get` and label-name read (see [ipc-security.md](ipc-security.md)); the spawn ops moved into `spawnv` |
| 11 | `quota(buf)` | per-uid usage/limit block |
| 12 | `display(op, ...)` | display grant (see [display.md](display.md)) |
| 13 | `tasks(buf)` | read-only scheduler snapshot (`task/introspect.rs`; MCP bridge phase 2) |
| 14 | `system_stats(op, buf, cap)` | uptime, idle ticks, frame/slab/heap counters, block-cache and slab frames (v5) and the task table for `top`/`sysmond`/System Monitor (`sysinfo.rs`, #144) |
| 15-20 | `stat`, `readdir`, `write_file`, `mkdir`, `unlink`, `rename` (path args) | path-based native VFS calls for the shell (`process/fsops.rs`, #6); FAT is read-only (`-EROFS`), `/tmp` is writable; `-errno` on failure |
| 21 | `power(op, arg)` | `0` reboot, `1` power-off (sync, then stop), `2` arm the shutdown watchdog (`arg` = the stop the kernel forces 30 s later if the machine is still up); `CAP_SYS_ADMIN` only, checked before the op; `init` is the only caller, after its orderly shutdown ([../shutdown.md](../shutdown.md); `process/power.rs`, `power_watchdog.rs`) |
| 22 | `fsync(path)` | flush the mount holding the file to its block device (`process/fsops.rs`, #260) |
| 23 | `dev(op, a1, a2, a3, a4)` | userspace driver access: list, claim, map_bar, pio, cfg, irq, release; `CAP_DEV_CLAIM` (`dev/syscall.rs`, #240; see [devices.md](devices.md)) |
| 24 | `wall_time(op, a1)` | UTC wall clock for native services: `get` returns centiseconds since the epoch, `set` steps it to `a1` seconds (`CAP_SYS_TIME`, checked before the argument; `process/wallsys.rs`, #369) |
| 25 | `input_raw(op, a1, a2)` | the raw input event bus for `inputd`: `open` (raises the consumer to `Interactive`), `poll` (24-byte HID-coded key events with a gapless `seq`, `Dropped` markers and a TSC-interpolated `ts_ns`), `close`; a publication rings the consumer's doorbell, which the Messenger `wait` op parks on (`WAIT_RAW_INPUT`); `CAP_INPUT_RAW` (`input/rawsys.rs`; see [../input-plan.md](../input-plan.md)) |
| 27 | `inet(op, a1, a2, a3)` | the `AF_INET` pump: `netd` (root or `_netd` only) fetches the socket requests the kernel queued for Linux programs, answers them, and moves bytes through the kernel's side of each socket (`process/inetsys.rs`, `ipc/inet/`, networking plan N5) |
| 26 | `random(buf, len)` | up to 256 bytes from the kernel CSPRNG (`entropy.rs`) for native services such as `netd`; open to every task, no capability, `-EFAULT` on a bad destination (`process/randsys.rs`, networking plan N2) |
| 28 | `append_file(path, data, len)` | append up to 1 MiB to the end of a file, creating it when absent (`process/fsops.rs`); `write_file` of the first chunk plus one append per further chunk writes a file larger than one call, which the package manager `pkgd` needs for binaries |
| 29 | `kill(slot, sig)` | end one task by slot (what `spawn` returned) with signal 0 (probe), `SIGTERM` or `SIGKILL`; the sender must share the target's uid or hold `CAP_KILL`; `-ESRCH`/`-EPERM`/`-EINVAL`, no group or broadcast form (`process/killsys.rs`); `init` uses it to stop an app being removed |
| 30 | `read_at(path, request)` | read up to `len` bytes of a file at `offset` into `buf`, where `request` points at three `u64`s `[buf, len, offset]`; at most 1 MiB per call, 0 at the end of the file (`process/fsops.rs`). Unlike syscall 3 it never loads the whole file into the kernel heap, so `pkgd` streams packages of any size with it (`user::files::read_large`) |
| 31 | `spawnv(req)` | the argv-vector spawn (fs F3): path, `argv`, `envp`, personality and credential stamp in one request block; see below (`process/spawnv.rs`) |
| 32 | `chmod(path, mode)` | set the permission bits (`mode` holds only `0o7777` bits; any other bit is `-EINVAL`, not masked) through `Vfs::setattr`, the path the Linux `chmod` takes, so the rules are the same: owner or root (`-EPERM`), setgid dropped outside the file's group, `-EROFS` on a read-only mount, `-ENOENT`/`-EFAULT` for a bad path (`process/fsops.rs`); `pkgd` makes a package's `bin/` files `0755`, since native spawn needs an `x` bit |
| 34 | `mono_time(op, a1)` | the monotonic clock in nanoseconds: `now` (op 0) returns `arch::clock::monotonic_ns`, `sleep_until` (op 1) parks until that clock reaches `a1` and returns 0, or 1 when a signal ended it; open to every task, unknown op `-EINVAL` (`process/timesys.rs`; `sys::monotonic_ns`, `sys::sleep_ns`; docs/performance-plan.md P2) |

**Userspace side: `libs/lazyos-sys`** (issue #666). Every userspace
`int 0x80` goes through this one crate, for native `no_std` programs (`user`)
and static-musl `std` programs (xui apps, `rhai`, LazyRAD) alike: the gate is
dispatched by task, not by binary kind, so the same wrapper is right for both.
It holds the syscall numbers (`nr`), the single `asm!` (`raw`, `unsafe`
because an argument may be a pointer the kernel writes), the errno values,
the Messenger ABI blocks and op table with safe byte-level wrappers (`msg`,
plus `msg::OwnedHandle`, released on drop, and `msg::endpoint_fd`/`msg::Pollable`,
an endpoint as a pollable Linux descriptor: [endpoint-fd.md](endpoint-fd.md)),
and typed surfaces for each
syscall above (`display` with `DisplayGrant`, `cred`, `time`, `spawn`, `stats`
and the decoded `sysinfo` snapshot, `dev`, `input`, `inet`, `storage`,
`random`, `kill`, `process`). Features: `alloc` (spawn blocks, `sysinfo`),
`parcel` (`msg::parcel`: `libmessenger` call/send/reply and the registry's
`resolve`/`connect`/`register`/`unregister`/`list` from the generated stubs),
`devinspect` (the device inspection rows) and `std` (`errno::io_error`,
`Duration` deadlines). `user::sys` and `xui_app::sys` are re-exports under the
names their callers already use, and `rhai_lazy::msg::gate` is a `Bus` over
it. `cargo test -p lazyos-sys --all-features` checks the argument packing and,
in `tests/kernel_tables.rs`, that every number reaches the expected arm of
`native_dispatch` in `process/gate.rs`, that the kernel dispatches no number
the crate does not name, and that the Messenger ops and wait flags match
`ipc/syscalls/abi.rs` and `ipc/channels/recv/waitset.rs`. A new native
syscall gets its number and wrapper there, never a private `asm!` block.

- `spawnv` opens the ELF on the OS volume (`/system/bin/<name>`) and the
  loader streams it into the child (below), names the task after the file's basename and leaks one interned `&'static str`
  per distinct service name (at most 64; later spellings share the name
  `service`), and gives the child a copy of the *caller's* credentials before it
  can run; the child's `argv`/`envp` blocks are keyed by slot, written before
  it can run and forgotten when it is reaped. `fork`, `clone`
  and threads inherit the same way, and only a program the kernel itself starts
  begins as root. The credential gate (syscall 10) has three ops: `0` set,
  `1` get and `4` **label name** (`a1` = a label id, `a2` -> a `8 + 160` byte
  buffer that receives a length word and the label bytes; needs `CAP_SETUID`,
  or the id must be the caller's own label). Ops `2` and `3` were the
  command-line spawn-with-credentials and spawn-labelled; `spawnv`'s `As` and
  `AsLabelled` modes replaced them with the same checks, and the gate answers
  them `-EINVAL`. A label only ever goes from `0` to a value, once, on a child
  being created (`spawnv` `AsLabelled`) by a `CAP_SETUID` holder that is
  unlabelled (or already in that label); op `0` and an `As` spawn must keep the
  label they land on, so a task cannot relabel or unlabel itself or hand a peer
  another label (`-EPERM`). `load_image`
  maps `PT_LOAD` segments (prot from `PF_W`/`PF_X`, `File` VMA) and the stack
  eagerly at `USER_HEAP_BASE = 0x60_0000` / `USER_STACK_TOP = 0x0800_0000`
  (`USER_STACK_SIZE = 0x2_0000`).

**`spawnv`** (syscall 31, `process/spawnv.rs`, fs F3 / issue #507). The one
spawn entry point; it replaced the command-line spawns (6 and the credential
gate's ops 2/3, deleted with their last caller). Nothing is split, so a path or
an argument may contain spaces. `rdi` points at a 17-word (`u64`, little-endian)
request:

| Word | Field |
|---|---|
| 0, 1 | `path_ptr`, `path_len` (no NUL; at most 255 bytes) |
| 2, 3, 4 | `argv_ptr`, `argv_len`, `argc`: `argc` NUL-terminated strings, `argv[0]` included; at most 4 KiB and 64 strings |
| 5, 6, 7 | `envp_ptr`, `envp_len`, `envc`: `envc` NUL-terminated `KEY=VALUE` strings (non-empty key); at most 4 KiB and 64; `envc` 0 needs `envp_len` 0 |
| 8 | personality: `0` native, `1` Linux (replaces the `linux:` prefix) |
| 9 | cred: `0` inherit, `1` as (the credential words), `2` as labelled (the credential words and the label) |
| 10-14 | `uid`, `gid`, `caps`, `label_id`, `session`: the credential block of syscall 10 |
| 15, 16 | `label_ptr`, `label_len` (mode 2; 1..=160 bytes of UTF-8) |

Everything is validated before anything is allocated or loaded: a path over
255 bytes is `-ENAMETOOLONG`; a block over 4 KiB or a count over 64 is
`-E2BIG`; `argc` 0, a count that does not match the block's NULs, a block that
does not end in NUL, an `envp` entry without `KEY=`, a NUL or non-UTF-8 byte in
the path, non-UTF-8 `argv`/`envp` for a native child, or an unknown
personality/cred selector is `-EINVAL`; an empty path is `-ENOENT`; memory that
is not readable user memory is `-EFAULT`. The cred modes run exactly the
credential gate's checks (`CAP_SETUID`, no widening, the label rules; `-EPERM`/
`-EACCES`) before a task exists. Then the `noexec` check (`-EACCES`), the file
(`-ENOENT`), the child (`-ENOMEM`), the stamp, and the blocks. Returns the
child's pid. A native child reads its blocks through syscall 9
(`sys::args()`, `sys::env()`); a Linux child gets them on its start stack
byte for byte. The kernel's own boot spawns (`kernel/src/main.rs`) and the
Linux `execve` of a native program fill the same per-task block from an `argv`
vector (`process::set_task_argv`): `argv[0]` is the program path for a boot
spawn and the caller's `argv[0]` for `execve`, and no argument is ever split or
joined. Services that still parse one string read `sys::service_args()`
(`argv[1..]` joined by spaces).

**User faults** (`arch/fault.rs`, `arch/idt.rs`, #7). Every CPU exception is
classified by the saved CS: a ring-0 fault still halts with a diagnostic, but a
ring-3 fault (bad pointer, privileged instruction, #DE, #UD, ...) terminates the
faulting process (all threads of its address space) with status `128 + signal`
(#PF/#GP -> SIGSEGV 11, #DE -> SIGFPE 8, #UD -> SIGILL 4), posts `SIGCHLD`, and
the scheduler moves on. A `SIGSEGV` handler still gets the first chance for #PF.
The report printed before the process is ended (`arch/fault_report.rs`:
registers, the 16 code bytes at `rip`, the top of the stack, signal state and
the task's last syscalls) reads user memory only where a page is already
present (`user_ptr::try_resident_bytes`): a kernel or non-canonical `rip`
prints `??`, and the report never demand-pages or allocates for the task it
describes. Evidence: the `fault_*` kernel tests; `faultprobe` (`/system/bin/faultprobe`) is also
runnable by hand from BusyBox `sh` (see "Native programs from `sh`" below).

**Linux shim** (`process/linux/`, issues #55-#60; plan: [linux-abi-plan.md](../linux-abi-plan.md))

- `load` builds a Linux stack (argv/envp/auxv; `AT_CLKTCK = 100`) and returns
  `(entry, stack_top)`; `spawn_linux` registers the `brk`/`mmap` bumps.
- `linux_dispatch` implements a growing subset: file I/O (`read`, `write`,
  `openat`, `close`, `stat`/`fstat`/`newfstatat`/`statx`, `getdents`/`getdents64`,
  `readv`/`writev`, `preadv`/`pwritev` (and the `*2` forms),
  `lseek`, `pread64`/`pwrite64`, `truncate`/`ftruncate`, `fsync`/`fdatasync`/
  `syncfs`/`sync`, `statfs`/`fstatfs`, `dup`/`dup2`, `fcntl`, `ioctl`,
  `readlink`, `getcwd`/`chdir`/`fchdir` (per-task working directory, see
  [filesystem.md](filesystem.md)), `chmod`/`fchmod`/`fchmodat`, `chown`/`fchown`/`lchown`/
  `fchownat`, `utimensat`/`utime`/`utimes`/`futimesat`,
  `mkdir`/`mkdirat`, `rmdir`, `rename`/`renameat`, `unlink`/`unlinkat` with
  `AT_REMOVEDIR`), memory (`mmap`, `mprotect`, `munmap`, `mremap`, `brk`),
  signals (`rt_sigaction`, `rt_sigprocmask`, `rt_sigpending`, `rt_sigreturn`,
  `sigaltstack`; the mask and the alternate stack are per thread, see
  [wait-signals.md](wait-signals.md)),
  process (`clone`, `fork`, `execve`, `exit`, `exit_group`, `wait4`, `kill`,
  `tkill`, `tgkill`, `set_tid_address`, `futex`), ids/groups (`getpid`/`gettid`,
  `getppid`, `setpgid`, `setsid`, `getpgid`, `getsid`), pipes/sockets (`pipe`,
  `pipe2`, `socketpair`, `socket`/`bind`/`listen`/`accept`/`connect`/`shutdown`/
  `getsockname` for `AF_UNIX` stream and seqpacket, `sendto`/`recvfrom`,
  `sendmsg`/`recvmsg` with `SCM_RIGHTS`, `memfd_create`,
  `poll`, `epoll_create1`/`epoll_ctl`/`epoll_wait`, `eventfd2`), and time/misc
  (`nanosleep`, `clock_nanosleep`, `clock_gettime`/`clock_getres` (the
  monotonic clocks read `arch::clock::monotonic_ns`: PIT ticks plus the
  calibrated TSC's progress within the tick, so 1 ns resolution; realtime adds
  the wall-clock offset to it),
  `gettimeofday`, `getrandom`, `uname`, `access`, `umask`, `arch_prctl`,
  `sched_getaffinity`, `getcpu` (309), and the scheduler calls below).
  Everything else logs `ENOSYS <nr> <name>`; the bench's
  `coverage.py` summarises what real programs still hit.
- **Thread-directed signal count.** `tkill`/`tgkill` with a real signal add
  one to a counter on the sender's thread-group leader
  (`LinuxExtras::thread_signals`), shown as `LazyOS_ThreadSignals` in
  `/proc/self/status`; the `jsc` fixture's scripts read it and the judge
  requires it to be positive for `suspend` (`compat_threadsig_*count*`).
- **Scheduling attributes** (`process/linux/{schedattr,procattr}.rs`,
  `task/schedupdate.rs`). `sched_yield` (24), `sched_getparam` (143),
  `sched_getscheduler` (145), `sched_get_priority_max`/`min` (146, 147),
  `sched_setattr` (314) and `sched_getattr` (315). The plain policies
  (`SCHED_OTHER`, `SCHED_BATCH`, `SCHED_IDLE`) with a nice value are kept per
  thread and mapped onto the stride scheduler's per-task weight
  (`task/nice.rs`); `sched_setattr` reads the old state, decides and stores
  the state and the weight in one hold of the task-table lock
  (`task::sched_update`), so two threads of a user cannot pass the "cannot
  lower nice / leave `SCHED_IDLE` / clear `reset_on_fork`" checks against a
  stale state, and the stored nice value and the live weight never disagree
  after a `sched_setattr` (a task the CPU guard has demoted gets the new
  weight when its window ends; the native `task::set_priority` and
  `raise_priority`, which kernel code and tools call, reset the weight to the
  class default and leave the stored nice value alone). As in Linux
  `SCHED_IDLE` takes no nice value: it keeps the task's own and needs no
  privilege for the one asked. `sched_getattr` writes `min(size, 56)` bytes
  and an `E2BIG` always writes 56 back. The `pid` argument is a thread id (a task slot) checked on all 64
  bits (`EINVAL` above `INT_MAX`, `ESRCH` for an empty slot); without
  `CAP_SYS_ADMIN` a caller may change only tasks of its own process (same
  thread-group leader and uid), never another process of its uid such as a
  native service, while privileged callers reach any slot. Reading is open.
  `sched_setscheduler` (144), `sched_setparam` (142), `setpriority` (141) and
  `getpriority` (140) are not offered (`ENOSYS`, pinned by
  `compat_schedattr_other_setters_not_offered`), so no other call can move
  the policy or nice value behind `sched_getattr`. WTF calls `sched_setattr`
  directly; musl's `pthread_setschedparam` (`sched_setscheduler`) and
  `pthread_setschedprio` (`sched_setparam`) get `ENOSYS`. Tests: `compat_schedattr_*` (including the two-thread soak
  and the guard-demotion case).
- File I/O runs against the ABI's own mount table. In the configured layout it
  mounts the same volumes as the native table (ext2 `/`, `/home`, ramfs `/tmp`;
  `fs/mounts.rs`), with no overlay; only the legacy layout roots it at the
  copy-up overlay from [filesystem.md](filesystem.md) over the read-only FAT
  boot volume. Either way `O_CREAT`, `O_TRUNC`, `O_APPEND`, `O_EXCL`, and
  `O_DIRECTORY` open, `mkdir`/`rename`/`unlink`/`rmdir`, and descriptor writes
  succeed. Relative `*at` calls join a real directory descriptor's recorded
  path (which is how `std`'s `remove_dir_all` walk works). Only fabricated
  files (`/proc`, the `/etc` maps) and directory streams are still snapshots
  copied into the kernel heap at open; a regular file on any mount reads
  through, see below.
- **Read-through descriptors** (`fs/openfile.rs`, `process/linux/{vfsfd,filerw,
  filesys}.rs`, issues #334, #265). A regular file opened on any mount (ext2
  `/` and `/home`, a ramfs `/tmp` or `/transient`, the FAT `/boot`, a copy-up
  root; `fs::abi_read_through`) becomes `Fd::Vfs`: an
  `Arc<OpenFile>` holding a path, the offset and the access mode, with **no
  snapshot**. `read`/`write`/`pread64`/`pwrite64`/`lseek`/`fstat` go to the VFS
  at the offset, so a file is not bounded by the kernel heap and every opener
  sees every write at once. `dup`/`fork` share the one description, and with it
  the offset, as POSIX requires. Permissions are checked at `open` (`READ` here,
  `WRITE` and `O_TRUNC` in `open_path`); afterwards operations run as root, so a
  descriptor survives its owner dropping privilege. `O_APPEND` writes (and
  `pwrite64` on such a file, as on Linux) land at the current EOF; a read is
  staged in 64 KiB pieces, a write is not staged (short counts are legal).
  `unlink` of an open file renames it to a hidden `.unlinked-<n>` entry in its
  directory and the last close deletes it, `rename` retargets open files
  (including under a renamed directory), and renaming over an open file
  unlinks it the same way. A stop between unlink and last close leaves the
  hidden entry, as an orphan inode would; the next mount of an unclean `/data`
  reclaims it, and user calls cannot create a `.unlinked-` name (`EINVAL`). Writes on a read-only device answer
  `EROFS` from the write, not from `open`.
- **Inspection** (issue #348). `/proc/mounts`, `/proc/self/mounts` and
  `/proc/self/mountinfo` are fabricated by `process/linux/procfs.rs` from
  `fs::abi_mounts()` each time they are opened (source, mount point, type,
  `ro`/`rw`; a filesystem's `name()` ending in `(ro)` marks a read-only mount),
  which is what BusyBox `df` and `mount` read. `getdents` (78) and `getdents64`
  share `dents.rs`: both hand out whole records (`EINVAL` when the buffer cannot
  hold the next one), with `d_off` set to the next record's stream offset.
  `statx` (`statx.rs`) is built from the same `Attrs` as `stat`; it claims
  type, mode, nlink, uid, gid, ino, size, blocks and the access, modify and
  change times (whole seconds), and no birth time (no backend records it).
  `stat`/`fstat`/`statx` report the real owner and times from the VFS `Meta`. `preadv`/`pwritev`/`preadv2`/`pwritev2` (`iov.rs`) share the walk
  and the caps of `readv`/`writev` (1024 segments, no length past `isize::MAX`,
  a bad array is `EFAULT`); `RWF_DSYNC`/`RWF_SYNC`/`RWF_APPEND` are refused with
  `EOPNOTSUPP` and an offset of -1 means the descriptor position.
- **Attributes** (`process/linux/attr.rs`, issue #345). The eleven
  `chmod`/`chown`/`utime` shapes decode into one `AttrRequest` on a path or a
  descriptor; the rules (owner/root, setuid clearing, `UTIME_NOW`'s
  write-permission fallback, `EPERM` vs `EACCES`) are the VFS's
  ([filesystem.md](filesystem.md), "Attributes"), so every shape answers the
  same. The descriptor forms (`fchmod`, `fchown`, `utimensat` with a `NULL`
  path, `AT_EMPTY_PATH`) work on `/data` descriptors and on snapshot
  descriptors that record a path, check ownership at call time without
  re-searching the path, and answer `EINVAL` for a pipe, socket or device
  node. `-1` ids leave an id alone; `UTIME_OMIT` keeps a stamp; a `tv_nsec`
  outside `0..1e9` (other than the two sentinels) or a `tv_usec` outside
  `0..1e6` is `EINVAL`; unknown `*at` flags are `EINVAL`. There are no symlinks
  yet, so `lchown` is `chown` and `AT_SYMLINK_NOFOLLOW` is accepted and changes
  nothing. A fabricated entry (`/bin`, `/dev`, an applet alias) answers `EROFS`.
  `stat`/`fstat`/`newfstatat` report `st_uid`, `st_gid` and the three times of
  a real node (whole seconds), and `fstat` of a snapshot descriptor re-reads
  its file while the path still names the inode it opened, so a later `chmod`
  shows.
- `truncate`/`ftruncate` (any mount; the descriptor must be writable),
  `fsync`/`fdatasync` (flush the one mount holding the file), `syncfs`, and
  `sync` (every mount) reach `Filesystem::flush`; `statfs`/`fstatfs` report the
  backend's `StatFs` (ext2 from the superblock, ramfs and the overlay from their
  caps). `pread64`/`pwrite64`/`ftruncate` also work on snapshot descriptors.
- Honored `clone` flags: `CLONE_VM`, `CLONE_SETTLS`, `CLONE_PARENT_SETTID`,
  `CLONE_CHILD_SETTID`, `CLONE_CHILD_CLEARTID`, `CLONE_FILES`, `CLONE_FS`
  (`CLONE_SIGHAND`, `CLONE_SYSVSEM`, `CLONE_VFORK`, `CLONE_DETACHED` are
  accepted as implied or moot; any other flag is `EINVAL`). `CLONE_VM` with
  `CLONE_THREAD` is a pthread; without it, musl's `posix_spawn` vfork child
  (a copy-on-write process with the parent's descriptor table); without
  `CLONE_VM`, a fork. A thread starts with a *copy* of its creator's
  descriptor table; with `CLONE_FILES` the two stay identical: every change
  (open, close, `dup2`, `FD_CLOEXEC`) is mirrored into each live member of the
  share group under the task-table lock (`task/fdshare.rs`, groups in
  `task/linuxstate.rs`), each copy holding its own reference, so a close in
  one thread closes everywhere and a thread's exit drops only its own copies.
  `CLONE_FS` mirrors `chdir` the same way. `execve` leaves both groups.
- `getpid` reports the thread group (the leader's slot), `gettid` the slot.
  `wait4`/`waitid` honour the pid argument (one child, any, own group,
  `-pgid`; `ECHILD` when none qualifies) and report a signal death as
  `WIFSIGNALED` with the core bit (the signal is recorded with the task when
  the signal layer ends it, `LinuxExtras::term_signal`). `vfork` is a fork.
- Futexes (`futex.rs`, `futex_queue.rs`): one waiter table keyed by (address
  space, address) with a bitset per waiter; `WAIT` (relative timeout),
  `WAIT_BITSET` (absolute, `FUTEX_CLOCK_REALTIME` honoured), `WAKE`,
  `WAKE_BITSET`, `REQUEUE`, `CMP_REQUEUE`, `WAKE_OP`; anything else `ENOSYS`.
  A thread's exit walks its `set_robust_list` list (owner-died + one wake).
- Terminals (`kernel/src/tty/`, `process/linux/tty.rs`): one line discipline
  type serves the console window (kept with its root task, keys fed in when
  someone reads or polls) and pseudo-terminals (`/dev/ptmx` + `/dev/pts/N`).
  `TCGETS`/`TCSETS*`, canonical editing and echo, `VMIN`/`VTIME`, `ISIG` to
  the foreground group `TIOCSPGRP` set (the console's `^C` is turned into
  `SIGINT` by the keyboard path unless `ISIG` is off), window size with
  `SIGWINCH`, `TIOCSCTTY`; `/dev/tty` is the controlling pty when there is
  one. Only terminals answer the terminal `ioctl`s (`ENOTTY` otherwise).
- Advisory locks (`locks.rs`): `flock` per open file description, `fcntl`
  record locks per process (and the OFD forms), released at unlock or when the
  owner is gone (checked lazily); `F_SETLKW` waits with a 100 ms re-check.
- Pipes are bounded 64 KiB byte rings with reader/writer refcounts, blocking
  waits on the task wait queues, EOF when the last writer closes and `-EPIPE`
  when the last reader closes (no SIGPIPE; see the module docs). `F_GETFL`/
  `F_SETFL` carry `O_NONBLOCK`, `F_GETFD`/`F_SETFD` carry `FD_CLOEXEC`, and
  `execve` closes marked descriptors while `fork` inherits them.
- `execve` replaces the image, resets the dispositions (the process's signal
  state is keyed by its PML4), keeps the calling thread's blocked mask and
  pending signals as Linux does but not its alternate stack, and tears down
  the old PML4 when it has no other users. The ABI bench injects `/system/bin/abi-init` via
  `LAZYOS_INIT`; `/system/bin/busybox` runs BusyBox `sh` on the shim, and results are
  generated into `docs/compat/` (git-ignored) by `tools/abi/run.py`.
- `execve` of a `#!` script (`process/linux/shebang.rs`, issue #491) runs the
  interpreter named on its first line with argv `[interp, (one-arg), script,
  argv[1..]]`, Linux `binfmt_script` style: the line is capped at 256 bytes,
  blanks (and the carriage return of a CRLF line) are trimmed, and an empty,
  NUL-carrying, over-long or non-UTF-8-interpreter line is `ENOEXEC`. Interpreters may be scripts up to
  4 hops deep (`ELOOP` past that); every hop gets the `noexec` and execute-bit
  checks, and a missing interpreter is `ENOENT`. This is what makes
  `chmod +x s.rhai; ./s.rhai` run under `#!/bin/rhai`.

**Native programs from `sh`** (`process/linux/native.rs`, issue #315)

How the kernel tells the two ABIs apart: it does not look at the image. Both a
native LazyOS program and a static musl program are x86_64 ET_EXEC files at the
same base, so a task's personality (`task::Kind::Native` vs `Kind::Linux`) is
fixed by *how it was started*: `spawn`/`spawn_child` build a native task,
`spawn_linux*` (`spawnv`'s Linux personality) and `fork`/`clone` build Linux ones. A
native task talks through `int 0x80` and prints with syscall 1 (`write`), which
the kernel appends to the terminal buffer of the task's root ancestor (and to
serial); it has no start stack (its `argv`/`envp` blocks come from syscall 9) and reads keys
with syscall 2.

BusyBox `sh` runs a command with `fork` + `execve`, so `execve` has to start a
native program without loading it over the Linux image. `sys_execve` first asks
`native::lookup(path)`. Programs live at their real names in `/system/bin`
(F3), and a list of the native ones (`top`, `confctl`, `messengerctl`,
`faultprobe`, `beep`, `pkgctl`, `powerctl`, ...) maps the name a user types
(`top`, `/bin/top`, found through the synthetic `/bin` that `$PATH` searches,
only while no real file has that path) or the full path (`/system/bin/top`)
to `/system/bin/<name>`, byte for byte: `TOP` and `top.elf` are not found.
An alias table adds the names that differ from the file: `msgctl` is
`messengerctl`, and `shutdown`/`poweroff`/`halt`/`reboot` run `powerctl` with
a preset first argument. The list exists because nothing in the ELF tells a
native program from a Linux one; every other file in `/system/bin` (`busybox`,
`rhai`, the xui apps) goes through the Linux loader, which resolves an
applet-shaped name to `/system/bin/<name>` before the BusyBox applet alias. A
native program this image does not ship is left to the Linux loader too. On a
match the calling task, which is `sh`'s expendable fork child:

1. checks the execute bit, reads the ELF from `/system/bin` and joins
   `argv[1..]` into the string syscall 9 hands the program (`E2BIG` past 4 KiB);
2. spawns it as a native child named after the file (`top`;
   `task::spawn_child_inheriting_fds`), which copies
   the caller's descriptor table except `FD_CLOEXEC` entries (`EAGAIN` when no
   task slot is free, `ENOMEM`, `ENOEXEC` for an image that will not load, all
   without leaking the slot or the half-built address space);
3. parks on the child-exit queue until *that* child exits (`reap_child_slot`)
   and calls `exit_group` with its status, so `sh`'s `wait4` sees the program's
   exit code (`128 + signal` if it faulted) exactly as for a Linux command.

Output: native `write` follows descriptor 1 when it is not the terminal
(`native::write_redirected`), so `cmd > file`, `cmd | grep x` and `cmd >/dev/null`
work for native *output*, both at the console and in the desktop Terminal (whose
stdout is a pipe). Limits, all by design of the minimum viable version:

- native *input* follows descriptor 0 when it is not the terminal: `read_char`
  returns one byte of the shell's stdin pipe or file (the desktop Terminal's
  keystrokes arrive that way), and a newline at end of input so a line reader
  ends instead of hanging. Only `read_char` reads it (no stderr; everything
  is descriptor 1);
- the program gets the shell's `argv` item for item (an argument with spaces
  stays whole), but no environment;
- there is no controlling tty and no job control, so `^C` is not delivered to
  the foreground job (a Linux `sleep` in the desktop Terminal has the same
  limit), a background job (`top &`) is not stopped by `SIGTTIN` when it reads
  the terminal, and its output interleaves with the prompt. Backgrounding
  otherwise works: the fork child does the spawn and the wait, the prompt
  returns at once, `wait`/`jobs` report the program's status. If the fork child
  is killed while it waits, the native program is orphaned to the kernel task
  and runs on until it exits (its slot is reclaimed then); a handled signal
  delivered to a *waiting* fork child kills the program;
- a program that is not in the table cannot be launched from `sh`; add a row to
  `NATIVE` for a new command-line tool.

Tests: `kernel/src/tests/native_exec_suite/` (name lookup and shadowing, the
argument line, descriptor/argument inheritance and redirected output, exit-status
propagation and reaping, `ENOEXEC`/`ENOENT`/`EAGAIN` without leaks, the `&`
lifecycle, and a 384-cycle spawn/exit soak that checks slots and frames) and the
`tools/screenshot/examples/native_exec.json` console session in `ci.yml`.

**Descriptor passing and shared memory** (issue #771). `sendmsg` with an
`SCM_RIGHTS` control message (`process/linux/scm.rs`) clones the caller's
descriptors into a `pipe::Rights` that the direction pipe of the `AF_UNIX`
socket queues beside the message's bytes (`ipc/pipe/rights.rs`: a `Record`
per message with its stream positions; `Ring` counts bytes in and out).
`recvmsg` takes the records whose first byte it consumed and installs the
descriptors in the receiver's table (`scm::deliver`), `FD_CLOEXEC` with
`MSG_CMSG_CLOEXEC`, `MSG_CTRUNC` for what does not fit. Descriptors are only
ever dropped with the ring unlocked, since closing a socket end takes other
rings' locks; the last reader closing a direction purges its queue
(`Pipe::release`). In-flight descriptors are charged to the sender's `Fds`
quota and a global ceiling (`ETOOMANYREFS`).

`memfd_create` makes an `Fd::Memfd` (`task/fdtypes.rs`) over a
`mem::memfd::Memfd`: a sparse vector of frames, so it is mappable, not a
`Vec<u8>` on the kernel heap like the scratch ramfs's files. `mmap`
(`process/linux/mem.rs`, `mmapfile.rs`) maps the frames eagerly with
`pte::SHARED` for `MAP_SHARED` (VMA `Kind::Shared`, whose pages the memfd
charges once) or read-only copy-on-write for `MAP_PRIVATE`; `fork`
(`mem/cow.rs`) copies a `SHARED` leaf as it is instead of marking it COW.
`MAP_SHARED | MAP_ANONYMOUS` (`Kind::SharedAnon`) is the same without a file.
The details and limits are in [linux-abi-plan.md](../linux-abi-plan.md),
round 4; tests are `linux_compat_suite/{scmrights,memfd}.rs` and the
`scmrights` fixture.

**Status.** Working: BusyBox `sh`, the 15 static musl fixtures in
`tools/abi/fixtures` (`persist` boots twice on one data disk; threads, `std::process` with piped stdio, `mremap`,
epoll/eventfd, `UnixStream`/seqpacket; matrix published by CI), native
supervision loop (`init`, app `Launch`), and real programs built from pinned
sources (`tools/linuxapps/build.py`, `LAZYOS_LINUXAPPS=1`): dash, sqlite3,
jq and ripgrep, each a bench row, plus the `compat` fixture. Gaps: dynamic
linking; `rt_sigtimedwait`; `timerfd`/`signalfd`/`inotify`;
`alarm`/`setitimer`; `SIGTTIN`/`SIGTTOU`; memfd seals and `MAP_SHARED` of
regular files; the umask is global, not per process. `SCM_RIGHTS`,
`memfd_create` and `MAP_SHARED` memfd mappings exist (issue #771, see
"Descriptor passing and shared memory" below). `link`/`symlink` answer `EPERM` (no
filesystem stores links). The `persist` fixture also sets and re-checks mode,
owner and times across its two boots. Kernel tests for the round-3 calls:
`kernel/src/tests/linux_compat_suite/`.
