//! The Terminal's pseudo-terminal: open `/dev/ptmx`, size it to the grid,
//! and start the shell on the slave as its controlling terminal.
//!
//! With a real tty the shell and everything it runs see what they would on
//! Linux: `isatty` is true, `tcsetattr` changes the kernel's line discipline
//! (cooked input with echo for `cat` or `dash`, raw for BusyBox's line editor
//! and `vi`), `^C` reaches the foreground job as `SIGINT`, and `TIOCGWINSZ`
//! reports the grid.

use std::ffi::CStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

/// The master side the Terminal reads output from and writes keys to.
pub struct Master {
    file: File,
}

impl Master {
    pub fn fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    pub fn file(&mut self) -> &mut File {
        &mut self.file
    }
}

/// Open a pseudo-terminal of `rows` x `cols` and spawn `command` on it
/// (stdin, stdout and stderr all the slave; a new session whose controlling
/// terminal it is). The parent keeps only the master.
pub fn spawn(mut command: Command, rows: u16, cols: u16) -> io::Result<(Master, Child)> {
    // SAFETY: `posix_openpt` takes flags and returns a new descriptor or -1.
    let master_fd = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `master_fd` is a fresh descriptor this process owns; the `File`
    // takes over closing it.
    let master = Master {
        file: unsafe { File::from_raw_fd(master_fd) },
    };
    // SAFETY: plain calls on a descriptor we own.
    if unsafe { libc::grantpt(master_fd) } != 0 || unsafe { libc::unlockpt(master_fd) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCSWINSZ` reads one `struct winsize` from a valid pointer.
    unsafe { libc::ioctl(master_fd, libc::TIOCSWINSZ, &size) };
    let mut name = [0 as libc::c_char; 64];
    // SAFETY: `ptsname_r` writes a NUL-terminated name into the buffer we pass.
    if unsafe { libc::ptsname_r(master_fd, name.as_mut_ptr(), name.len()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `ptsname_r` succeeded, so `name` holds a NUL-terminated string.
    let path = unsafe { CStr::from_ptr(name.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    let slave = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(&path)?;
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: the closure runs in the forked child before `exec` and only
    // makes async-signal-safe system calls: a new session, then the slave
    // (already its stdin) becomes that session's controlling terminal.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The command (and its copies of the slave) is dropped with this frame,
    // so once the shell exits the master reads end with `EIO`.
    drop(command);
    Ok((master, child))
}
