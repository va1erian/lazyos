//! `execve` of a native LazyOS program (issue #315).
//!
//! BusyBox `sh` starts every command with `fork` + `execve` on the Linux ABI,
//! but LazyOS's own programs (`top`, `confctl`, `msgctl`, ...) are *native*
//! ELFs that speak the `int 0x80` syscall set. The two cannot be told apart
//! from the image: both are static x86_64 executables at the same base (see
//! `process::spawnv`'s personality word). The kernel learns a task's personality only from
//! how it was started, so `execve` decides by *name*: [`NATIVE`] lists the
//! native programs in `/system/bin` a shell may run, matched either by the
//! short name a user types (`top`, found through the synthetic `/bin` that
//! `$PATH` searches, which means `/system/bin/top`) or by the full path
//! (`/system/bin/top`). Names are compared byte for byte: `TOP` and `top.elf`
//! are not found. [`ALIASES`] adds the names that differ from the file.
//!
//! A match does not replace the image. The calling task (the shell's fork
//! child, which is expendable) spawns the program as a native child, parks
//! until it exits and then exits with the program's status, so `sh`'s own
//! `wait4` sees the native exit code exactly as for a Linux command. The
//! child inherits the caller's descriptor table (minus `FD_CLOEXEC` entries)
//! and native `write` follows descriptor 1 when it is not the terminal
//! (`process::sys_write`), so redirections and pipes of native *output* work;
//! native *input* (`read_char`) follows descriptor 0 the same way
//! (`process::sys_read_char`), so the desktop Terminal's keystrokes, which
//! arrive on `sh`'s stdin pipe, reach the program; and the `argv` vector
//! reaches the program as it was given (syscall 9's per-task block, the one
//! `spawnv` fills), so an argument containing spaces stays one argument.
//!
//! Errors follow `execve(2)`: `ENOENT` when the program is not on the
//! volume, `ENOEXEC` when it will not load, `EAGAIN` when no task slot is free,
//! `E2BIG` for an oversized argument list. The shell survives all of them: it
//! only ever sees the errno from its own child.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FsError, Id};
use crate::ipc::pipe;
// A native program gets `spawnv`'s limits: the largest `argv` block (NULs
// included) and the most strings (`E2BIG` beyond either).
use crate::process::spawnv::{ARGC_MAX, ARGV_MAX};
use crate::task::signal::{self, SigInfo};
use crate::task::{self, FdKind, SpawnError, WakeReason};

use super::errno::{err, fs_err, E2BIG, EAGAIN, ENOENT, ENOEXEC, ENOMEM, ESRCH};
use super::procctl::sys_exit_group;

/// The native programs in `/system/bin` a shell can run, each found by its
/// own name (`top` is `/system/bin/top`). The list is needed because nothing
/// in the ELF tells a native program from a Linux one; every other file in
/// `/system/bin` (`busybox`, `rhai`, the xui apps) runs through the Linux
/// loader. Only programs that make sense from a command line belong here;
/// services (`init`, `keyd`, ...) are started by `init`.
const NATIVE: &[&str] = &[
    fhs::bin::TOP,
    fhs::bin::CONFCTL,
    fhs::bin::MESSENGERCTL,
    fhs::bin::FAULTPROBE,
    // The audio client (docs/driver-plan.md D6): `beep [freq_hz [ms [volume%]]]`.
    fhs::bin::BEEP,
    // The tracker-module player (docs/tracker-plan.md): `modplay <file.mod>`.
    fhs::bin::MODPLAY,
    // The mixer's volume control (docs/audio-plan.md): `mixer [master <%> |
    // mute | unmute | stream <id> <%>]`.
    fhs::bin::MIXER,
    // The package manager's command line (docs/packages.md): `pkgctl inspect |
    // install | remove | list`.
    fhs::bin::PKGCTL,
    // The NIC control tool (docs/networking-plan.md N1): `nicctl [arp]` shows
    // the card and its counters. On the image only with `LAZYOS_NET=1`.
    fhs::bin::NICCTL,
    // The device viewer (issue #481): `devctl [devices | rules | denials]`.
    // On the image with any userspace driver.
    fhs::bin::DEVCTL,
    // The network stack's tools (docs/networking-plan.md N2): `netctl [addr |
    // route | stats | ...]` and `ping <a.b.c.d> [count]`. On the image only
    // with `LAZYOS_NETD=1`.
    fhs::bin::NETCTL,
    fhs::bin::PING,
    // Sockets and names (docs/networking-plan.md N3): `nc [-u] [-l] <host>
    // <port> [text]` and `nslookup <name>`. On the image only with
    // `LAZYOS_NETD=1`.
    fhs::bin::NC,
    fhs::bin::NSLOOKUP,
    // Bulk TCP throughput (docs/performance-plan.md P4): `netbulk <host>
    // <port> <bytes>`, against `tools/net/bulk.py`'s server.
    fhs::bin::NETBULK,
    // The FTP client (N4): `ftp <host>[:port] [cmd ; cmd ...]`.
    fhs::bin::FTP,
    // The power command (docs/shutdown.md): `powerctl poweroff|reboot [-f]
    // [reason]` asks `init` for an orderly stop.
    fhs::bin::POWERCTL,
    // The in-memory user-space filesystem (docs/smb-plan.md F1): `memfuse
    // [-r] [-s MiB] [name]` serves `/mnt/<name>`, usually run with `&`.
    fhs::bin::MEMFUSE,
];

/// Names that differ from the file: `(name typed at the prompt, program,
/// fixed first argument)`. `msgctl` is the short name of `messengerctl`. The
/// power commands all go through `powerctl` to `init`'s orderly shutdown
/// (docs/shutdown.md); BusyBox's own `reboot`/`poweroff` applets would signal
/// a pid 1 that LazyOS does not have, so they are never reached.
const ALIASES: &[(&str, &str, &str)] = &[
    ("msgctl", fhs::bin::MESSENGERCTL, ""),
    ("shutdown", fhs::bin::POWERCTL, "poweroff"),
    ("poweroff", fhs::bin::POWERCTL, "poweroff"),
    ("halt", fhs::bin::POWERCTL, "poweroff"),
    ("reboot", fhs::bin::POWERCTL, "reboot"),
];

/// The directories a `$PATH` search (BusyBox `ash`'s default with no `$PATH`
/// is `/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`) or a
/// hand-typed path reaches a command through, written as `lookup` sees them:
/// no leading slash, trailing slash. The Linux path resolver also takes these
/// as the only directories where a BusyBox applet alias exists.
pub(super) const BIN_DIRS: &[&str] = &[
    "bin/",
    "sbin/",
    "usr/bin/",
    "usr/sbin/",
    "usr/local/bin/",
    "usr/local/sbin/",
];

/// `fhs::SYSTEM_BIN` as `lookup` sees a directory: no leading slash, trailing
/// slash. Only [`ALIASES`] names resolve there without a file.
pub(super) const SYSTEM_BIN_DIR: &str = "system/bin/";

/// The exit status reported when the child vanished without being reaped (it
/// cannot normally happen; `126` is the shell's "cannot execute").
const LOST_CHILD_STATUS: u64 = 126;

/// The `/system/bin` program `path` names, when it is a native one whose file
/// is on the volume.
///
/// The program's own path (`/system/bin/top`) always counts, because running
/// that file as a Linux program can only crash it. A short name (`top`,
/// `/bin/top`, only in the [`BIN_DIRS`]) or an [`ALIASES`] name only counts
/// while nothing real lives at that path, so a user's own script or binary
/// named `top` is never shadowed. Every comparison is byte for byte.
pub(crate) fn lookup(path: &str) -> Option<&'static str> {
    let file = match NATIVE.iter().find(|file| **file == path) {
        Some(file) => file,
        None => short_name(path)?,
    };
    is_regular_file(file).then_some(file)
}

/// The native program a short name in a [`BIN_DIRS`] directory (or with no
/// directory) stands for, or an [`ALIASES`] name in `/system/bin`, unless a
/// real file lives at `path`.
fn short_name(path: &str) -> Option<&'static str> {
    let trimmed = path.trim_start_matches('/');
    let (dir, base) = match trimmed.rfind('/') {
        Some(split) => trimmed.split_at(split + 1),
        None => ("", trimmed),
    };
    // A session's `PATH` is `/system/bin` (issue #508), so `$PATH` lookups of
    // an alias name reach it there too; a native program's own name there is
    // its file already (`lookup`).
    let in_system_bin = dir == SYSTEM_BIN_DIR;
    if !(dir.is_empty() || in_system_bin || BIN_DIRS.contains(&dir)) {
        return None;
    }
    let alias = ALIASES
        .iter()
        .find(|(name, _, _)| *name == base)
        .map(|&(_, file, _)| file);
    if in_system_bin && alias.is_none() {
        return None;
    }
    let file = alias.or_else(|| {
        NATIVE
            .iter()
            .copied()
            .find(|file| fhs::bin::name(file) == base)
    })?;
    let real_file_exists = !matches!(
        crate::fs::abi_stat(Id::current(), path),
        Err(FsError::NotFound)
    );
    (!real_file_exists).then_some(file)
}

/// Whether `path` is a regular file on the volume (a program this image does
/// not ship is left to the Linux loader, and its BusyBox applet alias).
fn is_regular_file(path: &str) -> bool {
    matches!(
        crate::fs::abi_stat(Id::current(), path),
        Ok(meta) if meta.kind == vfs::FileKind::File
    )
}

/// The fixed first argument an [`ALIASES`] name adds (`reboot` -> `"reboot"`);
/// empty for every other path, full program paths included.
pub(crate) fn preset_args(path: &str) -> &'static str {
    let base = path.rsplit('/').next().unwrap_or(path);
    ALIASES
        .iter()
        .find(|(name, _, _)| *name == base)
        .map_or("", |&(_, _, args)| args)
}

/// The `argv` a native program `execve`d as `path` receives: the caller's
/// `argv` item for item (each without its NUL terminator), with an [`ALIASES`]
/// preset (`reboot` -> `powerctl reboot`) inserted after `argv[0]`, and
/// `path` as `argv[0]` when the caller passed none. Nothing is split or
/// joined. A native program reads its arguments as text, so bytes that are
/// not UTF-8 are replaced, never refused. `None` (`E2BIG`) beyond `spawnv`'s
/// limits.
pub(crate) fn exec_argv(path: &str, argv: &[Vec<u8>]) -> Option<Vec<String>> {
    let text = |arg: &[u8]| {
        let arg = arg.split(|byte| *byte == 0).next().unwrap_or(&[]);
        String::from_utf8_lossy(arg).into_owned()
    };
    let mut items = Vec::with_capacity(argv.len() + 1);
    items.push(
        argv.first()
            .map_or_else(|| String::from(path), |arg0| text(arg0)),
    );
    let preset = preset_args(path);
    if !preset.is_empty() {
        items.push(String::from(preset));
    }
    items.extend(argv.iter().skip(1).map(|arg| text(arg)));
    let bytes: usize = items.iter().map(|item| item.len() + 1).sum();
    (items.len() as u64 <= ARGC_MAX && bytes <= ARGV_MAX).then_some(items)
}

/// The errno `execve` reports for a failed native spawn.
fn spawn_errno(error: SpawnError) -> u64 {
    match error {
        SpawnError::NoSlot => EAGAIN,
        SpawnError::NoMemory => ENOMEM,
        SpawnError::NoParent => ESRCH,
        SpawnError::BadImage(_) => ENOEXEC,
    }
}

/// Start `elf` (the program at `file`) as a native child of the calling task,
/// inheriting its descriptors, and record its `argv` (`argv[0]` included). The
/// task is named after the file (`/system/bin/top` runs as `top`). Returns the
/// child's slot, or the errno (as a positive value) for a failed spawn.
pub(crate) fn spawn<A: AsRef<[u8]>, I: crate::process::image::Image + ?Sized>(
    file: &'static str,
    elf: &I,
    argv: &[A],
) -> Result<usize, u64> {
    let slot = task::spawn_child_inheriting_fds(fhs::bin::name(file), elf).map_err(spawn_errno)?;
    // Set before the child can run: the syscall path holds interrupts off, so
    // no tick can schedule the child between the spawn and this store.
    crate::process::set_task_argv(slot, argv);
    Ok(slot)
}

/// Park until the child in `slot` exits and return its status.
///
/// Only that child is reaped, whatever else the caller has forked. An
/// interrupting signal (the terminal's `^C` reaches the whole process group,
/// which includes the child) makes the wait kill the child once so it cannot
/// outlive the interrupted shell command; the loop then still reaps it, so its
/// slot and frames are always released.
pub(crate) fn wait_for(slot: usize) -> u64 {
    let mut killed = false;
    loop {
        if let Some(status) = task::reap_child_slot(slot) {
            return status;
        }
        if !task::is_child(slot) {
            return LOST_CHILD_STATUS;
        }
        match task::wait_child_exit() {
            WakeReason::Woken | WakeReason::TimedOut => {}
            WakeReason::Interrupted => {
                if !killed {
                    killed = true;
                    let _ = signal::kill(
                        task::current(),
                        slot as i64,
                        signal::SIGKILL,
                        SigInfo::kernel(),
                    );
                }
                // The caller itself is being killed: it dies at its syscall
                // return and its child is re-parented, so stop waiting.
                if signal::killed(task::current()) {
                    return LOST_CHILD_STATUS;
                }
            }
        }
    }
}

/// `execve` for a native program: `None` when `path` is not one (the caller
/// carries on with the Linux loader), otherwise the errno of a failed launch.
/// On success it never returns: the caller exits with the program's status.
pub(crate) fn try_exec(path: &str, argv: &[Vec<u8>]) -> Option<u64> {
    let file = lookup(path)?;
    // `file` is never a synthetic name: it is the real `/system/bin` file that
    // runs, so it is checked where it is read (the native table), exactly as
    // native `spawn` checks it: its mount decides `noexec` first, and a missing
    // node is `ENOENT`, not a pass.
    if let Err(error) = crate::process::exec_perm::native(file) {
        return Some(fs_err(error));
    }
    // Streamed into the child's address space, never held whole here.
    let Ok(elf) = crate::process::image::VfsFile::native(Id::current(), file) else {
        return Some(err(ENOENT));
    };
    let Some(argv) = exec_argv(path, argv) else {
        return Some(err(E2BIG));
    };
    let slot = match spawn(file, &elf, &argv) {
        Ok(slot) => slot,
        Err(errno) => return Some(err(errno)),
    };
    let status = wait_for(slot);
    Some(sys_exit_group(status & 0xff))
}

/// Native syscall 1 (`write`) for a task whose descriptor 1 is not the
/// terminal: the bytes go through the Linux descriptor path, so a pipe, socket
/// or file the shell installed receives them. `None` means "descriptor 1 is
/// the terminal, use the console path".
///
/// A stream write can be short (`write_stream` moves one chunk), and native
/// callers do not loop, so this does: it returns the count written, or the
/// native failure code `u64::MAX` when nothing could be.
pub(crate) fn write_redirected(ptr: u64, len: u64) -> Option<u64> {
    if task::fd_kind(1) == FdKind::Terminal {
        return None;
    }
    let mut done = 0u64;
    while done < len {
        let result = super::io::sys_write(1, ptr.wrapping_add(done), len - done) as i64;
        if result < 0 {
            return Some(if done == 0 { u64::MAX } else { done });
        }
        if result == 0 {
            break;
        }
        done += result as u64;
    }
    Some(done)
}

/// The byte a native `read_char` (syscall 2) returns at end of input on a
/// redirected stdin: a newline, so a program reading a line ends it instead of
/// spinning on a stream that will never deliver another key.
const EOF_CHAR: u64 = b'\n' as u64;

/// Native syscall 2 (`read_char`) for a task whose descriptor 0 is not the
/// terminal: one byte from the pipe, socket or file the shell installed, so a
/// native program run from the desktop Terminal (whose keystrokes arrive on
/// `sh`'s stdin pipe, not the kernel key queue) can read them (issue #315).
/// `None` means "descriptor 0 is the terminal, use the key queue".
///
/// A blocking stream parks inside the read; a non-blocking one is polled,
/// napping between reads so the timer and the writer can run while each read
/// (the task table, the pipe) still runs with interrupts off (issue #382).
pub(crate) fn read_redirected() -> Option<u64> {
    match task::fd_kind(0) {
        FdKind::Terminal => None,
        // A `SOCK_SEQPACKET` read of one byte would truncate the message and
        // discard the rest, so message sockets are treated as ended input.
        FdKind::Socket if task::fd_seqpacket(0) => Some(EOF_CHAR),
        FdKind::Pipe | FdKind::Socket => {
            let mut byte = [0u8; 1];
            Some(task::poll_until(|| {
                // A killed task must reach its syscall return to die; this
                // loop never parks, so no `Interrupted` wake would end it.
                if task::signal::killed(task::current()) {
                    return Some(EOF_CHAR);
                }
                match task::fd_stream_read(0, &mut byte) {
                    Ok(1) => Some(u64::from(byte[0])),
                    Err(pipe::Error::WouldBlock) => None,
                    // End of stream, a signal, or a broken descriptor: nothing
                    // more will arrive.
                    _ => Some(EOF_CHAR),
                }
            }))
        }
        FdKind::File => Some(match task::fd_read(0, 1) {
            Some(chunk) if !chunk.is_empty() => u64::from(chunk[0]),
            _ => EOF_CHAR,
        }),
        _ => Some(EOF_CHAR),
    }
}
