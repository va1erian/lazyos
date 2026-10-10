//! Terminal descriptors from the syscall side: reads and writes through the
//! line discipline (the console's and the pseudo-terminals'), the terminal
//! `ioctl`s, and opening `/dev/ptmx`, `/dev/pts/<n>` and `/dev/tty`.
//!
//! Supported `ioctl`s on a terminal (console or pty slave; most also on a
//! master, which addresses its slave's settings): `TCGETS`, `TCSETS`,
//! `TCSETSW`, `TCSETSF` (flushes input), `TCFLSH`, `TCSBRK`/`TCXONC` (no-ops:
//! output is never held), `TIOCGWINSZ`/`TIOCSWINSZ` (a change sends
//! `SIGWINCH` to the foreground group), `TIOCGPGRP`/`TIOCSPGRP`, `TIOCSCTTY`,
//! `TIOCNOTTY`, `TIOCGSID`, `TIOCOUTQ`, `FIONREAD`; on a master `TIOCGPTN` and
//! `TIOCSPTLCK` (what `ptsname`/`unlockpt` use). Anything else is `ENOTTY`,
//! and every one of them is `ENOTTY` on a descriptor that is not a terminal.

use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::task::{self, consoletty, Fd};
use crate::tty::pty::{self, Pty};
use crate::tty::{termios, Ldisc, Termios, WinSize};
use crate::user_ptr;

use super::errno::{err, EAGAIN, EBADF, EFAULT, EINTR, EINVAL, EIO, ENOTTY, ENXIO};
use super::fd::fd_result;
use super::jobctl;

/// Bytes staged per terminal read or write.
const CHUNK: usize = 4096;
const SIGWINCH: u8 = 28;

/// The terminal a descriptor reaches.
enum Tty {
    Console,
    Pty { pty: Arc<Pty>, master: bool },
}

fn tty_of(fd: u64) -> Result<Tty, u64> {
    match task::fd_clone(fd as usize) {
        Some(Fd::Terminal) => Ok(Tty::Console),
        Some(Fd::Pty { ref pty, master }) => Ok(Tty::Pty {
            pty: Arc::clone(pty),
            master,
        }),
        Some(Fd::Closed) | None => Err(err(EBADF)),
        Some(_) => Err(err(ENOTTY)),
    }
}

/// Run `f` on the discipline behind `tty`.
fn with_ldisc<R>(tty: &Tty, f: impl FnOnce(&mut Ldisc) -> R) -> R {
    match tty {
        Tty::Console => {
            let (result, fed) = consoletty::with_console(f);
            consoletty::apply_fed(fed);
            result
        }
        Tty::Pty { pty, .. } => pty.with_ldisc(f),
    }
}

/// Read the console terminal through its discipline. Blocks for a line
/// (canonical) or a byte (raw, `VMIN` > 0); `VMIN` 0 waits at most `VTIME`.
pub(super) fn read_console(ptr: u64, len: u64) -> u64 {
    if len == 0 {
        return 0;
    }
    let mut buf = alloc::vec![0u8; (len as usize).min(CHUNK)];
    let mut deadline = None;
    loop {
        let (outcome, fed) = consoletty::with_console(|ldisc| {
            if ldisc.readable() {
                return Some(ldisc.read(&mut buf));
            }
            let t = ldisc.termios;
            if !t.canonical() && t.cc[termios::VMIN] == 0 {
                let vtime = u64::from(t.cc[termios::VTIME]);
                let due = *deadline.get_or_insert(task::ticks() + vtime * 10);
                if vtime == 0 || task::ticks() >= due {
                    return Some(0);
                }
            }
            None
        });
        consoletty::apply_fed(fed);
        if let Some(n) = outcome {
            return match user_ptr::try_copy_to(ptr, &buf[..n]) {
                Ok(()) => n as u64,
                Err(_) => err(EFAULT),
            };
        }
        // A key may have gone to another window: spurious wakeups loop.
        let woken = match deadline {
            Some(due) => task::wait::TERMINAL.wait(task::current(), Some(due)),
            None => task::wait_terminal(),
        };
        if woken == task::WakeReason::Interrupted {
            return err(EINTR);
        }
    }
}

fn pty_error(error: pty::Error) -> u64 {
    match error {
        pty::Error::WouldBlock => err(EAGAIN),
        pty::Error::Interrupted => err(EINTR),
        pty::Error::HungUp => err(EIO),
    }
}

/// `read` on a pty descriptor.
pub(super) fn read_pty(fd: u64, ptr: u64, len: u64) -> u64 {
    let Ok(Tty::Pty { pty, master }) = tty_of(fd) else {
        return err(EBADF);
    };
    if len == 0 {
        return 0;
    }
    let mut buf = alloc::vec![0u8; (len as usize).min(CHUNK)];
    let nonblock = pty.nonblock(master);
    let got = if master {
        pty.master_read(&mut buf, nonblock)
    } else {
        pty.slave_read(&mut buf, nonblock)
    };
    match got {
        Ok(n) => match user_ptr::try_copy_to(ptr, &buf[..n]) {
            Ok(()) => n as u64,
            Err(_) => err(EFAULT),
        },
        Err(error) => pty_error(error),
    }
}

/// One byte from pty descriptor `fd` (a native `read_char` whose stdin the
/// shell pointed at its pty slave); `None` at end of input or on an error.
pub(super) fn read_pty_char(fd: u64) -> Option<u64> {
    let Ok(Tty::Pty { pty, master }) = tty_of(fd) else {
        return None;
    };
    let mut byte = [0u8; 1];
    let nonblock = pty.nonblock(master);
    let got = if master {
        pty.master_read(&mut byte, nonblock)
    } else {
        pty.slave_read(&mut byte, nonblock)
    };
    matches!(got, Ok(1)).then(|| u64::from(byte[0]))
}

/// `write` on a pty descriptor: typed input on the master, program output on
/// the slave.
pub(super) fn write_pty(fd: u64, ptr: u64, len: u64) -> u64 {
    let Ok(Tty::Pty { pty, master }) = tty_of(fd) else {
        return err(EBADF);
    };
    let want = (len as usize).min(CHUNK);
    let bytes: Vec<u8> = match user_ptr::try_bytes(ptr, want) {
        Ok(bytes) => bytes.to_vec(),
        Err(_) => return err(EFAULT),
    };
    if master {
        let (n, signals) = pty.master_write(&bytes);
        for (signal, foreground) in signals {
            crate::tty::signal_foreground(foreground, crate::tty::signal_number(signal));
        }
        return n as u64;
    }
    match pty.slave_write(&bytes, pty.nonblock(false)) {
        Ok(n) => n as u64,
        Err(error) => pty_error(error),
    }
}

/// Open `/dev/ptmx`: a new pseudo-terminal's master, its slave locked until
/// `TIOCSPTLCK` (`unlockpt`).
pub(super) fn open_ptmx() -> u64 {
    match Pty::open_master(super::creds::ids()) {
        Some(pty) => fd_result(task::fd_open(Fd::pty_side(pty, true))),
        None => err(super::errno::ENOSPC),
    }
}

/// Open `/dev/pts/<n>`, the slave: only its owner or root may
/// ([`jobctl::may_open_slave`]). Without `O_NOCTTY` it becomes the caller's
/// controlling terminal when the job-control rules allow
/// ([`jobctl::acquire_on_open`]).
pub(super) fn open_pts(name: &str, flags: u64) -> u64 {
    const O_NOCTTY: u64 = 0o400;
    let Ok(index) = name.parse::<u32>() else {
        return err(super::errno::ENOENT);
    };
    let Some(pty) = Pty::find_slave(index) else {
        return err(ENXIO);
    };
    if let Err(code) = jobctl::may_open_slave(&pty) {
        return code;
    }
    if flags & O_NOCTTY == 0 {
        jobctl::acquire_on_open(&pty);
    }
    fd_result(task::fd_open(Fd::pty_side(pty, false)))
}

/// Open `/dev/tty`: the caller's controlling pty if it has one, else its
/// console window.
pub(super) fn open_tty() -> u64 {
    match task::linuxstate::ctty() {
        Some(pty) => fd_result(task::fd_open(Fd::pty_side(pty, false))),
        None => fd_result(task::fd_open(Fd::Terminal)),
    }
}

fn read_termios(arg: u64) -> Result<Termios, u64> {
    let bytes = user_ptr::try_bytes(arg, termios::SIZE).map_err(|_| err(EFAULT))?;
    let mut raw = [0u8; termios::SIZE];
    raw.copy_from_slice(bytes);
    Ok(Termios::from_bytes(&raw))
}

fn put_u32(arg: u64, value: u32) -> u64 {
    match user_ptr::try_write::<u32>(arg, value) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}

/// A terminal `ioctl` on `fd`; `ENOTTY` for any other descriptor.
pub(super) fn ioctl(fd: u64, request: u64, arg: u64) -> u64 {
    let tty = match tty_of(fd) {
        Ok(tty) => tty,
        Err(code) => return code,
    };
    match request {
        0x5401 => {
            let bytes = with_ldisc(&tty, |l| l.termios.to_bytes());
            match user_ptr::try_copy_to(arg, &bytes) {
                Ok(()) => 0,
                Err(_) => err(EFAULT),
            }
        }
        0x5402..=0x5404 => match read_termios(arg) {
            Ok(new) => with_ldisc(&tty, |l| {
                if request == 0x5404 {
                    l.flush_input();
                }
                l.set_termios(new);
                0
            }),
            Err(code) => code,
        },
        0x540B => match arg {
            0 | 2 => with_ldisc(&tty, |l| {
                l.flush_input();
                0
            }),
            1 => 0, // TCOFLUSH: output is never held back
            _ => err(EINVAL),
        },
        0x5409 | 0x540A => 0, // TCSBRK / TCXONC: nothing is buffered or stopped
        0x5411 => put_u32(arg, 0), // TIOCOUTQ
        0x5413 => {
            let size = with_ldisc(&tty, |l| l.winsize);
            let words = [size.rows, size.cols, 0, 0];
            let mut bytes = [0u8; 8];
            for (i, word) in words.iter().enumerate() {
                bytes[i * 2..i * 2 + 2].copy_from_slice(&word.to_le_bytes());
            }
            match user_ptr::try_copy_to(arg, &bytes) {
                Ok(()) => 0,
                Err(_) => err(EFAULT),
            }
        }
        0x5414 => {
            let (Ok(rows), Ok(cols)) = (
                user_ptr::try_read::<u16>(arg),
                user_ptr::try_read::<u16>(arg + 2),
            ) else {
                return err(EFAULT);
            };
            let (changed, foreground) = with_ldisc(&tty, |l| {
                let new = WinSize { rows, cols };
                let changed = l.winsize != new;
                l.winsize = new;
                (changed, l.foreground())
            });
            if changed {
                // Either way only the foreground group's tasks still in the
                // terminal's session get it (the console's is its window's).
                match &tty {
                    Tty::Pty { .. } => crate::tty::signal_foreground(foreground, SIGWINCH),
                    Tty::Console => crate::tty::signal_console(
                        crate::tty::Foreground {
                            session: consoletty::console_session(),
                            ..foreground
                        },
                        SIGWINCH,
                    ),
                }
            }
            0
        }
        0x540F => {
            let group = with_ldisc(&tty, |l| l.fg_pgrp);
            put_u32(arg, if group != 0 { group } else { task::pgid() } as u32)
        }
        0x5410 => {
            let Ok(group) = user_ptr::try_read::<i32>(arg) else {
                return err(EFAULT);
            };
            match &tty {
                Tty::Pty { pty, .. } => jobctl::set_pty_fg(pty, group),
                Tty::Console => jobctl::set_console_fg(group),
            }
        }
        0x540E => match &tty {
            // TIOCSCTTY: become the controlling terminal (the session
            // leader only); the caller's group is the foreground one.
            Tty::Pty { pty, master: false } => jobctl::set_ctty(pty),
            Tty::Pty { master: true, .. } => err(ENOTTY),
            Tty::Console => jobctl::set_console_ctty(),
        },
        0x5422 => match &tty {
            Tty::Pty { pty, .. } => jobctl::drop_ctty(pty), // TIOCNOTTY
            Tty::Console => {
                task::linuxstate::set_ctty(None);
                0
            }
        },
        0x5429 => match &tty {
            // TIOCGSID: the session the terminal controls.
            Tty::Pty { pty, .. } => match pty.with_ldisc(|l| l.session) {
                0 => err(ENOTTY),
                sid => put_u32(arg, sid as u32),
            },
            Tty::Console => put_u32(arg, consoletty::console_session() as u32),
        },
        0x541B => {
            let queued = match &tty {
                Tty::Console => with_ldisc(&tty, |l| l.available()),
                Tty::Pty { pty, master } => pty.queued(*master),
            };
            put_u32(arg, queued as u32)
        }
        0x8004_5430 => match &tty {
            Tty::Pty { pty, master: true } => put_u32(arg, pty.index()), // TIOCGPTN
            _ => err(ENOTTY),
        },
        0x4004_5431 => match &tty {
            Tty::Pty { pty, master: true } => match user_ptr::try_read::<i32>(arg) {
                Ok(lock) => {
                    pty.set_locked(lock != 0); // TIOCSPTLCK
                    0
                }
                Err(_) => err(EFAULT),
            },
            _ => err(ENOTTY),
        },
        _ => err(ENOTTY),
    }
}

/// Inode numbers of the terminal device nodes, the same whether a node is
/// reached by path (`stat`) or by descriptor (`fstat`): `ttyname` compares the
/// two to confirm the name `/proc/self/fd/N` gave.
const CONSOLE_INO: u64 = 4;
const PTMX_INO: u64 = 5;
const DATA_DEVICE_INO: u64 = 6;
const PTS_INO_BASE: u64 = 0x100;

/// The owner `fstat` and `stat` report for a pty slave (whoever opened
/// `/dev/ptmx`), as `(uid, gid)`; root for every other terminal node.
pub(super) fn fd_owner(fd: u64) -> (u32, u32) {
    match tty_of(fd) {
        Ok(Tty::Pty { pty, master: false }) => pty.owner(),
        _ => (0, 0),
    }
}

/// The inode `fstat` reports for a terminal descriptor.
pub(super) fn fd_ino(fd: u64) -> u64 {
    match tty_of(fd) {
        Ok(Tty::Pty { pty, master: false }) => PTS_INO_BASE + u64::from(pty.index()),
        Ok(Tty::Pty { master: true, .. }) => PTMX_INO,
        _ => CONSOLE_INO,
    }
}

/// The owner and permission bits `stat` reports for a device node path: a
/// pty slave is its owner's, mode 0620 (devpts); the others are root's and
/// world-usable.
pub(super) fn node_owner(path: &str) -> ((u32, u32), u16) {
    path.strip_prefix("/dev/pts/")
        .and_then(|n| n.parse::<u32>().ok())
        .and_then(Pty::find_slave)
        .map_or(((0, 0), 0o666), |pty| (pty.owner(), 0o620))
}

/// The inode `stat` reports for a device node path.
pub(super) fn path_ino(path: &str) -> u64 {
    if let Some(index) = path
        .strip_prefix("/dev/pts/")
        .and_then(|n| n.parse::<u64>().ok())
    {
        return PTS_INO_BASE + index;
    }
    match path {
        "/dev/ptmx" => PTMX_INO,
        "/dev/tty" | "/dev/console" | "/dev/tty0" | "/dev/tty1" => CONSOLE_INO,
        _ => DATA_DEVICE_INO,
    }
}
