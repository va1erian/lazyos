//! Job control on pseudo-terminals, under Linux's rules: who may open a
//! slave, which session a terminal controls, and which process group may be
//! its foreground.
//!
//! A terminal's own signals (`^C`, `SIGWINCH`, the hang-up) are sent by the
//! kernel, past every credential check, to the foreground group. So that
//! group must only ever be one the caller could signal by job control
//! anyway: a group of the session the terminal controls, set by a member of
//! that session through its controlling terminal.
//!
//! * Opening `/dev/pts/<n>` needs the slave's owner (whoever opened
//!   `/dev/ptmx`) or root, as devpts's mode 0620 allows (`EACCES`).
//! * A terminal becomes a session's controlling terminal when its session
//!   leader, holding none, opens it without `O_NOCTTY` or asks with
//!   `TIOCSCTTY`, and only while no other live session controls it (`EPERM`;
//!   Linux's root-only steal with argument 1 is not offered).
//! * `TIOCSPGRP` works only on the caller's controlling terminal, in the
//!   session it controls (`ENOTTY`), and only for a group of that session
//!   (`EPERM`). On the console the session is the window's.
//! * `setsid` drops the caller's controlling terminal; `TIOCNOTTY` from the
//!   session leader frees the terminal for another session.

use alloc::sync::Arc;

use crate::ipc::credentials;
use crate::task::{self, consoletty};
use crate::tty::pty::Pty;

use super::errno::{err, EACCES, EINVAL, ENOTTY, EPERM, ESRCH};

/// The caller's pid and session.
fn caller() -> (usize, usize) {
    let slot = task::current();
    (task::process::pid_of(slot), task::process::sid_of(slot))
}

/// Whether the caller may open `pty`'s slave: its owner, or root.
pub(super) fn may_open_slave(pty: &Pty) -> Result<(), u64> {
    let uid = credentials::of(task::current()).uid;
    if uid == 0 || uid == pty.owner().0 {
        Ok(())
    } else {
        Err(err(EACCES))
    }
}

/// Whether `pty` is the caller's controlling terminal.
fn is_ctty(pty: &Arc<Pty>) -> bool {
    task::linuxstate::ctty().is_some_and(|ctty| Arc::ptr_eq(&ctty, pty))
}

/// Make `pty` the caller's controlling terminal if the rules allow it: the
/// caller leads its session, holds no terminal, and no other live session
/// holds this one (`EPERM` otherwise); a leader already holding it succeeds.
fn acquire(pty: &Arc<Pty>) -> Result<(), u64> {
    let (pid, sid) = caller();
    if pid == sid && is_ctty(pty) && pty.with_ldisc(|l| l.session) == sid {
        return Ok(());
    }
    if pid != sid || task::linuxstate::ctty().is_some() {
        return Err(err(EPERM));
    }
    let group = task::pgid();
    let taken = pty.with_ldisc(|l| {
        if l.session != 0 && l.session != sid && task::process::session_alive(l.session) {
            return false;
        }
        l.session = sid;
        l.fg_pgrp = group;
        true
    });
    if !taken {
        return Err(err(EPERM));
    }
    task::linuxstate::set_ctty(Some(Arc::clone(pty)));
    Ok(())
}

/// A slave opened without `O_NOCTTY`: becomes the controlling terminal when
/// [`acquire`] allows, and is simply opened otherwise, as on Linux.
pub(super) fn acquire_on_open(pty: &Arc<Pty>) {
    let _ = acquire(pty);
}

/// `TIOCSCTTY` on a slave.
pub(super) fn set_ctty(pty: &Arc<Pty>) -> u64 {
    match acquire(pty) {
        Ok(()) => 0,
        Err(code) => code,
    }
}

/// `TIOCSCTTY` on the console: the window's session leader takes the
/// foreground; nothing else may.
pub(super) fn set_console_ctty() -> u64 {
    let (pid, sid) = caller();
    if pid != sid || consoletty::console_session() != sid {
        return err(EPERM);
    }
    let group = task::pgid();
    let (_, fed) = consoletty::with_console(|l| l.fg_pgrp = group);
    consoletty::apply_fed(fed);
    0
}

/// Check a `TIOCSPGRP` target for a caller in session `sid`.
fn check_group(group: i32, sid: usize) -> Result<usize, u64> {
    if group <= 0 {
        return Err(err(EINVAL));
    }
    match task::process::group_session(group as usize) {
        None => Err(err(ESRCH)),
        Some(session) if session != sid => Err(err(EPERM)),
        Some(_) => Ok(group as usize),
    }
}

/// `TIOCSPGRP` on a pty (either side addresses the same terminal).
pub(super) fn set_pty_fg(pty: &Arc<Pty>, group: i32) -> u64 {
    let (_, sid) = caller();
    if !is_ctty(pty) || pty.with_ldisc(|l| l.session) != sid {
        return err(ENOTTY);
    }
    match check_group(group, sid) {
        Ok(group) => pty.with_ldisc(|l| {
            l.fg_pgrp = group;
            0
        }),
        Err(code) => code,
    }
}

/// `TIOCSPGRP` on the console: the window's session only.
pub(super) fn set_console_fg(group: i32) -> u64 {
    let (_, sid) = caller();
    if consoletty::console_session() != sid {
        return err(ENOTTY);
    }
    match check_group(group, sid) {
        Ok(group) => {
            let (_, fed) = consoletty::with_console(|l| l.fg_pgrp = group);
            consoletty::apply_fed(fed);
            0
        }
        Err(code) => code,
    }
}

/// `TIOCNOTTY` on a pty: drop the caller's controlling terminal; from the
/// session leader, also free the terminal for another session.
pub(super) fn drop_ctty(pty: &Arc<Pty>) -> u64 {
    if !is_ctty(pty) {
        return err(ENOTTY);
    }
    let (pid, sid) = caller();
    if pid == sid {
        pty.with_ldisc(|l| {
            if l.session == sid {
                l.session = 0;
                l.fg_pgrp = 0;
            }
        });
    }
    task::linuxstate::set_ctty(None);
    0
}
