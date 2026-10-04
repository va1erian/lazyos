//! Terminals: the termios settings, the line discipline that applies them,
//! and pseudo-terminals.
//!
//! Two kinds of terminal use the same [`ldisc::Ldisc`]:
//!
//! * the console terminal of each kernel window (the descriptors `Fd::Terminal`
//!   stands for): keys arrive through the keyboard path, the discipline lives
//!   with the window's root task ([`crate::task::linuxstate`]);
//! * pseudo-terminals ([`pty`]): a master/slave pair a terminal emulator (the
//!   desktop Terminal) opens through `/dev/ptmx`, so the shell it hosts sees a
//!   real tty: echo, line editing, `^C` for the foreground job, a window size.

pub mod ldisc;
pub mod pty;
pub mod termios;

pub use ldisc::{Foreground, Ldisc, Signal};
pub use termios::{Termios, WinSize};

/// Send `signal` to a pseudo-terminal's foreground group, if it has one:
/// only to the group's tasks still in the session that controls the
/// terminal ([`crate::task::signal::kill_terminal_group`]). The sender is
/// the kernel (no credential check applies), which is why the group must
/// have been set under the job-control rules and must still belong there.
pub fn signal_foreground(foreground: Foreground, signal: u8) {
    let Foreground { group, session } = foreground;
    if group == 0 || session == 0 {
        return;
    }
    signal_console(foreground, signal);
}

/// [`signal_foreground`] for a console window, whose session may be the
/// kernel's own (0): the group's tasks in that session only.
pub fn signal_console(foreground: Foreground, signal: u8) {
    if foreground.group != 0 {
        let _ =
            crate::task::signal::kill_terminal_group(foreground.group, foreground.session, signal);
    }
}

/// The Linux signal number for a discipline signal.
pub fn signal_number(signal: Signal) -> u8 {
    match signal {
        Signal::Interrupt => crate::task::signal::SIGINT,
        Signal::Quit => 3,
        Signal::Suspend => 20,
    }
}
