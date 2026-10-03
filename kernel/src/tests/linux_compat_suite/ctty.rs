//! Pseudo-terminal ownership and job control: only the `/dev/ptmx` opener
//! (or root) opens a slave, only a session leader acquires a controlling
//! terminal no live session holds, and `TIOCSPGRP` takes only a group of the
//! caller's session on its own controlling terminal (the kernel later sends
//! that group the terminal's signals past every credential check).

use super::*;
use crate::ipc::credentials::{self, Cred};

const IOCTL: u64 = 16;
const STAT: u64 = 4;
const FSTAT: u64 = 5;
const SETPGID: u64 = 109;
const SETSID: u64 = 112;
const TIOCSCTTY: u64 = 0x540E;
const TIOCSPGRP: u64 = 0x5410;
const TIOCNOTTY: u64 = 0x5422;
const TIOCGSID: u64 = 0x5429;
const TIOCGPTN: u64 = 0x8004_5430;
const TIOCSPTLCK: u64 = 0x4004_5431;
const O_RDWR: u64 = 2;
const O_NOCTTY: u64 = 0o400;
const ESRCH: u64 = neg(3);
const EACCES: u64 = neg(13);
const ENOTTY: u64 = neg(25);

/// Fork the current task, running as `uid`.
fn fork_as(uid: u32) -> Result<usize, String> {
    let slot = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    credentials::set(slot, Cred::new(uid, uid, 0, 0, 0));
    Ok(slot)
}

/// A task leading a session `setsid` made (a task forked from the kernel
/// task already leads its own; its child calls `setsid`). Leaves it current.
fn new_session(uid: u32) -> Result<usize, String> {
    task::harness::switch_current(task::KERNEL_TASK);
    let parent = fork_as(uid)?;
    task::harness::switch_current(parent);
    let slot = fork_as(uid)?;
    task::harness::switch_current(slot);
    check!(sys(SETSID, &[]) == slot as u64, "setsid in {slot}");
    Ok(slot)
}

/// A new, unlocked pty: `(master, index)`.
fn new_pty() -> Result<(u64, u32), String> {
    let ptmx = cpath("/dev/ptmx");
    let master = sys(2, &[ptmx.as_ptr() as u64, O_RDWR, 0]);
    check!((master as i64) >= 3, "open /dev/ptmx: {master:#x}");
    let mut index = u32::MAX;
    check!(
        sys(IOCTL, &[master, TIOCGPTN, &mut index as *mut u32 as u64]) == 0,
        "TIOCGPTN"
    );
    let unlock = 0i32;
    check!(
        sys(IOCTL, &[master, TIOCSPTLCK, &unlock as *const i32 as u64]) == 0,
        "unlockpt"
    );
    Ok((master, index))
}

fn open_slave(index: u32, flags: u64) -> u64 {
    let name = cpath(&format!("/dev/pts/{index}"));
    sys(2, &[name.as_ptr() as u64, O_RDWR | flags, 0])
}

fn set_fg(fd: u64, group: usize) -> u64 {
    let group = group as i32;
    sys(IOCTL, &[fd, TIOCSPGRP, &group as *const i32 as u64])
}

fn tiocgsid(fd: u64) -> u64 {
    let mut sid = u32::MAX;
    match sys(IOCTL, &[fd, TIOCGSID, &mut sid as *mut u32 as u64]) {
        0 => u64::from(sid),
        code => code,
    }
}

/// `(mode, uid)` from a `struct stat` buffer.
fn mode_uid(stat: &[u8; 144]) -> (u32, u32) {
    let word = |at: usize| u32::from_le_bytes(stat[at..at + 4].try_into().unwrap());
    (word(24), word(28))
}

/// The slave belongs to whoever opened `/dev/ptmx`: another user is refused
/// (`EACCES`), root is not, and `stat`/`fstat` report the owner and 0620.
pub fn pty_slave_owner() -> Result<(), String> {
    fresh()?;
    let owner = fork_as(1000)?;
    let stranger = fork_as(1001)?;
    task::harness::switch_current(owner);
    let (master, index) = new_pty()?;
    let path = cpath(&format!("/dev/pts/{index}"));
    let mut stat = [0u8; 144];
    check!(
        sys(STAT, &[path.as_ptr() as u64, stat.as_mut_ptr() as u64]) == 0,
        "stat the slave"
    );
    check!(
        mode_uid(&stat) == (0o20620, 1000),
        "slave stat {:o}/{}",
        mode_uid(&stat).0,
        mode_uid(&stat).1
    );
    task::harness::switch_current(stranger);
    check!(
        open_slave(index, O_NOCTTY) == EACCES,
        "another user opened the slave"
    );
    task::harness::switch_current(owner);
    let slave = open_slave(index, O_NOCTTY);
    check!((slave as i64) >= 3, "the owner's open: {slave:#x}");
    let mut stat = [0u8; 144];
    check!(sys(FSTAT, &[slave, stat.as_mut_ptr() as u64]) == 0, "fstat");
    check!(mode_uid(&stat) == (0o20620, 1000), "slave fstat");
    sys(3, &[slave]);
    task::harness::switch_current(task::KERNEL_TASK);
    let by_root = open_slave(index, O_NOCTTY);
    check!((by_root as i64) >= 3, "root's open: {by_root:#x}");
    sys(3, &[by_root]);
    task::harness::switch_current(owner);
    sys(3, &[master]);
    task::harness::reset();
    check!(crate::tty::pty::Pty::live() == 0, "a pty leaked");
    Ok(())
}

/// The controlling terminal and foreground group follow Linux's session
/// rules, on a pty and on the console.
pub fn ctty_job_control() -> Result<(), String> {
    fresh()?;
    let leader = new_session(0)?;
    let (master, index) = new_pty()?;
    let slave = open_slave(index, 0);
    check!((slave as i64) >= 3, "open the slave: {slave:#x}");
    check!(
        tiocgsid(slave) == leader as u64,
        "the leader's open did not make it the controlling terminal"
    );
    let member = fork_as(0)?;
    task::harness::switch_current(member);
    check!(sys(SETPGID, &[0, 0]) == 0, "setpgid");
    check!(
        sys(IOCTL, &[slave, TIOCSCTTY, 0]) == EPERM,
        "a non-leader took the terminal"
    );
    task::harness::switch_current(leader);
    check!(set_fg(slave, member) == 0, "a group of the session");
    check!(
        set_fg(master, leader) == 0,
        "the same terminal by its master"
    );
    check!(set_fg(slave, 0) == EINVAL, "group 0");
    check!(set_fg(slave, 400) == ESRCH, "a missing group");
    // `setsid` leaves the controlling terminal behind.
    let detached = fork_as(0)?;
    task::harness::switch_current(detached);
    check!(sys(SETSID, &[]) == detached as u64, "detached setsid");
    check!(
        set_fg(slave, detached) == ENOTTY,
        "a new session kept the old controlling terminal"
    );
    sys(3, &[slave]);
    sys(3, &[master]);
    // Another session: it may open the slave, but neither steer nor take it.
    let outsider = new_session(0)?;
    let theirs = open_slave(index, 0);
    check!((theirs as i64) >= 3, "the outsider's open: {theirs:#x}");
    check!(
        set_fg(theirs, outsider) == ENOTTY,
        "TIOCSPGRP on someone else's terminal"
    );
    check!(
        sys(IOCTL, &[theirs, TIOCSCTTY, 0]) == EPERM,
        "a held terminal was taken"
    );
    task::harness::switch_current(leader);
    check!(
        set_fg(slave, outsider) == EPERM,
        "a group of another session became the foreground"
    );
    // The leader lets go: now the outsider's session may take it.
    check!(sys(IOCTL, &[slave, TIOCNOTTY, 0]) == 0, "TIOCNOTTY");
    check!(set_fg(slave, leader) == ENOTTY, "TIOCSPGRP after TIOCNOTTY");
    task::harness::switch_current(outsider);
    check!(
        sys(IOCTL, &[theirs, TIOCSCTTY, 0]) == 0,
        "a released terminal"
    );
    check!(tiocgsid(theirs) == outsider as u64, "the new session");
    check!(set_fg(theirs, outsider) == 0, "the new session's group");
    // The console (a kernel-started task's window): only groups of the
    // window's session.
    task::harness::switch_current(task::KERNEL_TASK);
    let window = fork_as(0)?;
    task::harness::switch_current(window);
    let job = fork_as(0)?;
    task::harness::switch_current(job);
    check!(sys(SETPGID, &[0, 0]) == 0, "console job setpgid");
    task::harness::switch_current(window);
    check!(set_fg(0, job) == 0, "a group of the console's session");
    check!(
        set_fg(0, outsider) == EPERM,
        "another session's group on the console"
    );
    for (slot, fds) in [(leader, [slave, master]), (outsider, [theirs, theirs])] {
        task::harness::switch_current(slot);
        sys(3, &[fds[0]]);
        sys(3, &[fds[1]]);
    }
    task::harness::switch_current(member);
    sys(3, &[slave]);
    sys(3, &[master]);
    task::harness::reset();
    check!(crate::tty::pty::Pty::live() == 0, "a pty leaked");
    Ok(())
}

/// Soak: a session leader acquires, steers and releases fresh terminals
/// hundreds of times; nothing leaks and it ends without a terminal.
pub fn ctty_soak() -> Result<(), String> {
    fresh()?;
    let leader = new_session(1000)?;
    for round in 0..300 {
        let (master, index) = new_pty()?;
        let slave = open_slave(index, 0);
        check!((slave as i64) >= 3, "round {round}: open {slave:#x}");
        check!(
            tiocgsid(slave) == leader as u64,
            "round {round}: not acquired"
        );
        check!(set_fg(master, leader) == 0, "round {round}: TIOCSPGRP");
        check!(
            sys(IOCTL, &[slave, TIOCNOTTY, 0]) == 0,
            "round {round}: TIOCNOTTY"
        );
        sys(3, &[slave]);
        sys(3, &[master]);
    }
    check!(task::linuxstate::ctty().is_none(), "a terminal stayed held");
    task::harness::reset();
    check!(
        crate::tty::pty::Pty::live() == 0,
        "{} ptys leaked",
        crate::tty::pty::Pty::live()
    );
    Ok(())
}
