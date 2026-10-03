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

pub use ldisc::{Ldisc, Signal};
pub use termios::{Termios, WinSize};

/// The Linux signal number for a discipline signal.
pub fn signal_number(signal: Signal) -> u8 {
    match signal {
        Signal::Interrupt => crate::task::signal::SIGINT,
        Signal::Quit => 3,
        Signal::Suspend => 20,
    }
}
