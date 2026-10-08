//! Native syscall numbers (`rax` for `int 0x80`), the userspace side of
//! `native_dispatch` in `kernel/src/process/gate.rs`.
//!
//! `tests/kernel_tables.rs` reads that file and fails when a number here is
//! not dispatched to the expected kernel handler, or when the kernel
//! dispatches a number missing from [`ALL`].

/// `exit(code)`: terminate the task.
pub const EXIT: u64 = 0;
/// `write(ptr, len)`: bytes to the console.
pub const WRITE: u64 = 1;
/// `read_char()`: block until a key is pressed.
pub const READ_CHAR: u64 = 2;
/// `read_file(name_z, buf, len)`: read a whole file.
pub const READ_FILE: u64 = 3;
/// `sbrk(increment)`: grow the heap.
pub const SBRK: u64 = 4;
/// `messenger(op, args, result)`: the Messenger fabric ([`crate::msg`]).
pub const MESSENGER: u64 = 5;
// 6 was `spawn(cmdline)`, replaced by `spawnv` (fs F3).
/// `wait(deadline)`: reap a child, `(pid << 32) | status`.
pub const WAIT: u64 = 7;
/// `clock()`: the PIT tick counter (100 Hz).
pub const CLOCK: u64 = 8;
/// `args(buf, len, which)`: this program's `argv` or `envp` block.
pub const ARGS: u64 = 9;
/// `creds(op, a1, a2)`: the audited credential gate ([`crate::cred`]).
pub const CREDS: u64 = 10;
/// `quota(buf)`: this task's resource usage.
pub const QUOTA: u64 = 11;
/// `display(op, a1, a2)`: the display grant ([`crate::display`]).
pub const DISPLAY: u64 = 12;
/// `tasks(buf)`: the scheduler task list.
pub const TASKS: u64 = 13;
/// `system_stats(op, a1, a2)`: the system monitor snapshot.
pub const SYSTEM_STATS: u64 = 14;
/// `stat(path, out)`.
pub const STAT: u64 = 15;
/// `readdir(path, buf, len)`.
pub const READDIR: u64 = 16;
/// `write_file(path, data, len)`.
pub const WRITE_FILE: u64 = 17;
/// `mkdir(path)`.
pub const MKDIR: u64 = 18;
/// `unlink(path)`.
pub const UNLINK: u64 = 19;
/// `rename(from, to)`.
pub const RENAME: u64 = 20;
/// `power(op, a1)`: reboot, power off, the shutdown watchdog.
pub const POWER: u64 = 21;
/// `fsync()`.
pub const FSYNC: u64 = 22;
/// `dev(op, a1, a2, a3, a4)`: device inspection and claims ([`crate::dev`]).
pub const DEV: u64 = 23;
/// `wall_time(op, a1)`: the UTC wall clock.
pub const WALL_TIME: u64 = 24;
/// `input_raw(op, a1, a2)`: the raw input bus ([`crate::input`]).
pub const INPUT_RAW: u64 = 25;
/// `random(buf, len)`: the kernel CSPRNG.
pub const RANDOM: u64 = 26;
/// `inet(op, a1, a2, a3)`: the `AF_INET` pump ([`crate::inet`]).
pub const INET: u64 = 27;
/// `append_file(path, data, len)`.
pub const APPEND_FILE: u64 = 28;
/// `kill(slot, sig)`.
pub const KILL: u64 = 29;
/// `read_at(path, buf, offset)`.
pub const READ_AT: u64 = 30;
/// `spawnv(request)`: start a program ([`crate::spawn`]).
pub const SPAWNV: u64 = 31;
/// `chmod(path, mode)`.
pub const CHMOD: u64 = 32;
/// `storage(op, a1, a2, a3, a4)`: block providers ([`crate::storage`]).
pub const STORAGE: u64 = 33;
/// `mono_time(op, a1)`: the monotonic clock and sleep.
pub const MONO_TIME: u64 = 34;
/// `fuse(op, a1, a2, a3, a4)`: user-space filesystems (`libs/fused`).
pub const FUSE: u64 = 35;

/// Every number above with the kernel handler `native_dispatch` routes it to
/// (a substring of the match arm), for `tests/kernel_tables.rs`.
pub const ALL: &[(u64, &str)] = &[
    (EXIT, "exit"),
    (WRITE, "sys_write"),
    (READ_CHAR, "sys_read_char"),
    (READ_FILE, "sys_read_file"),
    (SBRK, "sys_sbrk"),
    (MESSENGER, "ipc::syscalls::dispatch"),
    (WAIT, "sys_wait"),
    (CLOCK, "sys_clock"),
    (ARGS, "sys_args"),
    (CREDS, "sys_creds"),
    (QUOTA, "sys_quota"),
    (DISPLAY, "display::dispatch"),
    (TASKS, "sys_tasks"),
    (SYSTEM_STATS, "sysinfo::dispatch"),
    (STAT, "fsops::dispatch"),
    (READDIR, "fsops::dispatch"),
    (WRITE_FILE, "fsops::dispatch"),
    (MKDIR, "fsops::dispatch"),
    (UNLINK, "fsops::dispatch"),
    (RENAME, "fsops::dispatch"),
    (POWER, "fsops::dispatch"),
    (FSYNC, "fsops::dispatch"),
    (DEV, "dev::syscall::dispatch"),
    (WALL_TIME, "wallsys::dispatch"),
    (INPUT_RAW, "input::rawsys::dispatch"),
    (RANDOM, "randsys::dispatch"),
    (INET, "inetsys::dispatch"),
    (APPEND_FILE, "fsops::dispatch"),
    (KILL, "killsys::dispatch"),
    (READ_AT, "fsops::dispatch"),
    (SPAWNV, "sys_spawnv"),
    (CHMOD, "fsops::dispatch"),
    (STORAGE, "block::provider::sys::dispatch"),
    (MONO_TIME, "timesys::dispatch"),
    (FUSE, "fuse::sys::dispatch"),
];
