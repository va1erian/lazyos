# Processes, supervision & Linux ABI

**What it is.** Process identity and the process tree, the native supervision
syscalls (`spawn`/`wait`/`clock`/`args`/`creds`), the ELF loaders, and the Linux
syscall shim.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/task/process.rs` | Tree, groups, sessions, `finish`, `reap_child` |
| `kernel/src/process/mod.rs` (+ `creds.rs`, `spawn.rs`) | ELF loader, syscalls 6-11 (creds/quota/tasks in `creds.rs`, spawn in `spawn.rs`) |
| `kernel/src/process/gate.rs` | The `int 0x80` gate: register-save stub and the syscall routing table (native dispatch) |
| `kernel/src/process/linux/` | Linux ELF loader + syscall dispatch, futex, clone (split by syscall family; see its `mod.rs` doc comment) |
| `kernel/src/ipc/pipe.rs` | Pipes (`pipe`/`pipe2`) and `AF_UNIX` socket pairs |
| `kernel/src/arch/linux.rs` | `syscall`/`sysret` entry (see [arch.md](arch.md)) |
| `kernel/src/task/signal.rs` | `SIGCHLD`, group termination (see [wait-signals.md](wait-signals.md)) |

**Identity model** (`task/process.rs`, issue #59)

- `pid == scheduler slot`; slots are recycled. `pgid` and `sid` hold the pid of
  the leader. The tree is derived by scanning for `parent == slot` (cheap at
  `MAX_TASKS = 64`) rather than stored as child lists.
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
| 13 | `tasks(buf)` | read-only scheduler snapshot (`task/introspect.rs`; MCP bridge phase 2) |
| 14 | `system_stats(op, buf, cap)` | uptime, frame/slab/heap counters and the task table for `top`/`sysmond` (`sysinfo.rs`, #144) |
| 15-20 | `stat`, `readdir`, `write_file`, `mkdir`, `unlink`, `rename` (path args) | path-based native VFS calls for the shell (`process/fsops.rs`, #6); FAT is read-only (`-EROFS`), `/tmp` is writable; `-errno` on failure |
| 21 | `power(op)` | `reboot`/`shutdown`, `CAP_SYS_ADMIN` only (`process/power.rs`, #6) |
| 22 | `fsync(path)` | flush the mount holding the file to its block device (`process/fsops.rs`, #260) |
| 23 | `dev(op, a1, a2, a3, a4)` | userspace driver access: list, claim, map_bar, pio, cfg, irq, release; `CAP_DEV_CLAIM` (`dev/syscall.rs`, #240; see [devices.md](devices.md)) |
| 24 | `wall_time(op, a1)` | UTC wall clock for native services: `get` returns centiseconds since the epoch, `set` steps it to `a1` seconds (`CAP_SYS_TIME`, checked before the argument; `process/wallsys.rs`, #369) |
| 25 | `input_raw(op, a1, a2)` | the raw input event bus for `inputd`: `open`, `poll` (24-byte HID-coded key events with a gapless `seq` and `Dropped` markers), `close`; `CAP_INPUT_RAW` (`input/rawsys.rs`; see [../input-plan.md](../input-plan.md)) |

- `spawn` reads the ELF from the FAT image, leaks one interned `&'static str`
  per distinct service name (at most 64; later spellings share the name
  `service`), and gives the child a copy of the *caller's* credentials before it
  can run; `SERVICE_ARGS` is keyed by slot and cleared on reuse. `fork`, `clone`
  and threads inherit the same way, and only a program the kernel itself starts
  begins as root. `load_image`
  maps `PT_LOAD` segments (prot from `PF_W`/`PF_X`, `File` VMA) and the stack
  eagerly at `USER_HEAP_BASE = 0x60_0000` / `USER_STACK_TOP = 0x80_0000`
  (`USER_STACK_SIZE = 0x2_0000`).

**User faults** (`arch/fault.rs`, `arch/idt.rs`, #7). Every CPU exception is
classified by the saved CS: a ring-0 fault still halts with a diagnostic, but a
ring-3 fault (bad pointer, privileged instruction, #DE, #UD, ...) terminates the
faulting process (all threads of its address space) with status `128 + signal`
(#PF/#GP -> SIGSEGV 11, #DE -> SIGFPE 8, #UD -> SIGILL 4), posts `SIGCHLD`, and
the scheduler moves on. A `SIGSEGV` handler still gets the first chance for #PF.
Evidence: the `fault_*` kernel tests; `faultprobe` (`FAULTPRB.ELF`) is also
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
  signals (`rt_sigaction`, `rt_sigprocmask`, `rt_sigreturn`, `sigaltstack`),
  process (`clone`, `fork`, `execve`, `exit`, `exit_group`, `wait4`, `kill`,
  `tkill`, `tgkill`, `set_tid_address`, `futex`), ids/groups (`getpid`/`gettid`,
  `getppid`, `setpgid`, `setsid`, `getpgid`, `getsid`), pipes/sockets (`pipe`,
  `pipe2`, `socketpair`, `socket`/`bind`/`listen`/`accept`/`connect`/`shutdown`/
  `getsockname` for `AF_UNIX` stream and seqpacket, `sendto`/`recvfrom`,
  `poll`, `epoll_create1`/`epoll_ctl`/`epoll_wait`, `eventfd2`), and time/misc
  (`nanosleep`, `clock_nanosleep`, `clock_gettime`/`clock_getres`,
  `gettimeofday`, `getrandom`, `uname`, `access`, `umask`, `arch_prctl`,
  `sched_getaffinity`). Everything else logs `ENOSYS <nr> <name>`; the bench's
  `coverage.py` summarises what real programs still hit.
- File I/O runs against the ABI's own mount table, whose root is the copy-up
  overlay from [filesystem.md](filesystem.md), so `O_CREAT`, `O_TRUNC`,
  `O_APPEND`, `O_EXCL`, and `O_DIRECTORY` open, `mkdir`/`rename`/`unlink`/
  `rmdir`, and descriptor writes all succeed over the read-only FAT boot
  volume. Relative `*at` calls join a real directory descriptor's recorded
  path (which is how `std`'s `remove_dir_all` walk works); descriptors on the
  root and `/tmp` snapshot file bytes at open and `write` patches the snapshot
  after updating the backing file. Files on the persistent `/data` volume are
  different, see below.
- **Descriptors on `/data`** (`fs/openfile.rs`, `process/linux/{vfsfd,filerw,
  filesys}.rs`, issue #334). A regular file opened under `/data` (only when a
  data volume is mounted; `abi_persistent`) becomes `Fd::Vfs`: an
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

**Native programs from `sh`** (`process/linux/native.rs`, issue #315)

How the kernel tells the two ABIs apart: it does not look at the image. Both a
native LazyOS program and a static musl program are x86_64 ET_EXEC files at the
same base, so a task's personality (`task::Kind::Native` vs `Kind::Linux`) is
fixed by *how it was started*: `spawn`/`spawn_child` build a native task,
`spawn_linux*` (the `linux:` prefix) and `fork`/`clone` build Linux ones. A
native task talks through `int 0x80` and prints with syscall 1 (`write`), which
the kernel appends to the terminal buffer of the task's root ancestor (and to
serial); it has no `argv` (only the string syscall 9 returns) and reads keys
with syscall 2.

BusyBox `sh` runs a command with `fork` + `execve`, so `execve` has to start a
native program without loading it over the Linux image. `sys_execve` first asks
`native::lookup(path)`: a small table (`top`, `confctl`, `msgctl`/`messengerctl`,
`faultprobe`) maps the name a user types (`top`, `/bin/top`, found through the
synthetic `/bin` that `$PATH` searches, only while no real file has that path)
or the boot-volume name (`/TOP.ELF`) to the 8.3 `.ELF` file. On a match the
calling task, which is `sh`'s expendable fork child:

1. checks the execute bit, reads the ELF from the boot volume (`ENOENT` if the
   image does not ship it, e.g. `top` in the `LAZYOS_DESKTOP=1` image) and joins
   `argv[1..]` into the string syscall 9 hands the program (`E2BIG` past 4 KiB);
2. spawns it as a native child (`task::spawn_child_inheriting_fds`), which copies
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
- arguments are one whitespace-split string, so an argument that contains spaces
  is split; there is no environment;
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
  `PROGRAMS` for a new command-line tool.

Tests: `kernel/src/tests/native_exec_suite.rs` (name lookup and shadowing, the
argument line, descriptor/argument inheritance and redirected output, exit-status
propagation and reaping, `ENOEXEC`/`ENOENT`/`EAGAIN` without leaks, the `&`
lifecycle, and a 384-cycle spawn/exit soak that checks slots and frames) and the
`tools/screenshot/examples/native_exec.json` console session in `ci.yml`.

**Status.** Working: BusyBox `sh`, the 14 static musl fixtures in
`tools/abi/fixtures` (`persist` boots twice on one data disk; threads, `std::process` with piped stdio, `mremap`,
epoll/eventfd, `UnixStream`/seqpacket; matrix published by CI), native
supervision loop (`init`, app `Launch`). Gaps: `poll` edge cases, full
`SA_RESTART`, shared file tables (and `CLONE_FS`: a thread's `chdir` does not move its
siblings), dynamic linking. Still `ENOSYS` on the filesystem side: `link`/`symlink`. The
`persist` fixture also sets and re-checks mode, owner and times across its two
boots.
