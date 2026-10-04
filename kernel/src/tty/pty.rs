//! Pseudo-terminals: `/dev/ptmx` hands out a master, `/dev/pts/<n>` is its
//! slave. What the master writes is typed input (through the slave's line
//! discipline, which may echo it back and raise `^C`); what the slave writes is
//! the program's output, post-processed (`\n` -> `\r\n`) and queued for the
//! master to read. A terminal emulator holds the master, the shell it runs
//! holds the slave as its controlling terminal.
//!
//! Lifetime: each side counts its open descriptors (`dup`/`fork` copies
//! included). When the last master closes, the slave sees a hang-up (reads
//! return end-of-file, writes `EIO`) and the foreground group gets `SIGHUP`;
//! when the last slave closes after having been opened, the master's reads end
//! with `EIO`, as on Linux. At most [`MAX_PTYS`] pairs exist at once.

use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use spin::Mutex;

use crate::task::wait::WaitQueue;
use crate::task::{WaitKind, WakeReason};

use super::ldisc::{Foreground, Ldisc, Signal};

/// Most pseudo-terminals open at once (`/dev/pts/0..63`).
pub const MAX_PTYS: usize = 64;
/// Output a slave may queue before its writes block.
pub const OUTPUT_CAPACITY: usize = 16 * 1024;

/// Why a pty transfer did not move bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    WouldBlock,
    Interrupted,
    /// The other side is gone (`EIO`).
    HungUp,
}

struct State {
    ldisc: Ldisc,
    /// Slave output waiting for the master.
    output: VecDeque<u8>,
    /// `TIOCSPTLCK`: a locked pty's slave cannot be opened.
    locked: bool,
}

/// One master/slave pair.
pub struct Pty {
    index: u32,
    /// The `(uid, gid)` that opened `/dev/ptmx`: the slave's owner, the only
    /// user (besides root) who may open it, as devpts's mode 0620 allows.
    owner: (u32, u32),
    state: Mutex<State>,
    /// Slave readers waiting for input, and master writers never wait.
    input_wq: WaitQueue,
    /// Master readers waiting for output; slave writers waiting for room.
    output_wq: WaitQueue,
    masters: AtomicUsize,
    slaves: AtomicUsize,
    /// A slave was opened at least once (so its last close is a hang-up).
    slave_seen: AtomicBool,
    master_nonblock: AtomicBool,
    slave_nonblock: AtomicBool,
    /// Edge counters for `epoll` (master readable / slave readable).
    master_events: AtomicU64,
    slave_events: AtomicU64,
}

static PTYS: Mutex<Vec<(u32, Weak<Pty>)>> = Mutex::new(Vec::new());

impl Pty {
    /// A new pair with the lowest free index, owned by `owner`, or `None`
    /// when all are in use. The caller holds the master reference it is handed.
    pub fn open_master(owner: (u32, u32)) -> Option<Arc<Pty>> {
        let mut table = PTYS.lock();
        table.retain(|(_, weak)| weak.strong_count() > 0);
        if table.len() >= MAX_PTYS {
            return None;
        }
        let index = (0..MAX_PTYS as u32).find(|i| table.iter().all(|(used, _)| used != i))?;
        let pty = Arc::new(Pty {
            index,
            owner,
            state: Mutex::new(State {
                ldisc: Ldisc::new(),
                output: VecDeque::new(),
                locked: true,
            }),
            input_wq: WaitQueue::new(WaitKind::Pipe),
            output_wq: WaitQueue::new(WaitKind::Pipe),
            masters: AtomicUsize::new(0),
            slaves: AtomicUsize::new(0),
            slave_seen: AtomicBool::new(false),
            master_nonblock: AtomicBool::new(false),
            slave_nonblock: AtomicBool::new(false),
            master_events: AtomicU64::new(0),
            slave_events: AtomicU64::new(0),
        });
        table.push((index, Arc::downgrade(&pty)));
        Some(pty)
    }

    /// The pty behind `/dev/pts/<index>`, if it exists and is unlocked.
    pub fn find_slave(index: u32) -> Option<Arc<Pty>> {
        let table = PTYS.lock();
        let pty = table
            .iter()
            .find(|(used, _)| *used == index)
            .and_then(|(_, weak)| weak.upgrade())?;
        let usable = !pty.state.lock().locked && pty.masters.load(Ordering::Acquire) > 0;
        usable.then_some(pty)
    }

    /// Live pseudo-terminals (tests: a leak shows as a count that stays up).
    #[cfg(lazyos_tests)]
    pub fn live() -> usize {
        PTYS.lock()
            .iter()
            .filter(|(_, weak)| weak.strong_count() > 0)
            .count()
    }

    pub fn index(&self) -> u32 {
        self.index
    }

    /// The slave's owner, `(uid, gid)`.
    pub fn owner(&self) -> (u32, u32) {
        self.owner
    }

    /// Take a descriptor reference on one side.
    pub fn acquire(&self, master: bool) {
        if master {
            self.masters.fetch_add(1, Ordering::AcqRel);
        } else {
            self.slaves.fetch_add(1, Ordering::AcqRel);
            self.slave_seen.store(true, Ordering::Release);
        }
    }

    /// Drop a descriptor reference; the last one on a side hangs the other
    /// up. Returns the foreground to send `SIGHUP` to when the last master
    /// went away.
    pub fn release(&self, master: bool) -> Option<Foreground> {
        let counter = if master { &self.masters } else { &self.slaves };
        if counter.fetch_sub(1, Ordering::AcqRel) != 1 {
            return None;
        }
        self.wake_all();
        if master {
            Some(self.state.lock().ldisc.foreground())
        } else {
            None
        }
    }

    fn wake_all(&self) {
        self.master_events.fetch_add(1, Ordering::AcqRel);
        self.slave_events.fetch_add(1, Ordering::AcqRel);
        self.input_wq.notify_all();
        self.output_wq.notify_all();
        crate::task::notify_poll_key(self as *const Self as u64);
    }

    fn master_open(&self) -> bool {
        self.masters.load(Ordering::Acquire) > 0
    }

    fn slave_gone(&self) -> bool {
        self.slave_seen.load(Ordering::Acquire) && self.slaves.load(Ordering::Acquire) == 0
    }

    /// Run `f` on the line discipline (termios, window size, groups).
    pub fn with_ldisc<R>(&self, f: impl FnOnce(&mut Ldisc) -> R) -> R {
        let result = f(&mut self.state.lock().ldisc);
        // A settings change can make a read possible (leaving canonical mode).
        self.slave_events.fetch_add(1, Ordering::AcqRel);
        self.input_wq.notify_all();
        crate::task::notify_poll_key(self as *const Self as u64);
        result
    }

    pub fn set_locked(&self, locked: bool) {
        self.state.lock().locked = locked;
    }

    pub fn nonblock(&self, master: bool) -> bool {
        if master {
            self.master_nonblock.load(Ordering::Acquire)
        } else {
            self.slave_nonblock.load(Ordering::Acquire)
        }
    }

    pub fn set_nonblock(&self, master: bool, on: bool) {
        let flag = if master {
            &self.master_nonblock
        } else {
            &self.slave_nonblock
        };
        flag.store(on, Ordering::Release);
    }

    /// Typed input from the master: every byte goes through the discipline.
    /// Returns the bytes taken and the signals to raise (with the foreground).
    pub fn master_write(&self, src: &[u8]) -> (usize, Vec<(Signal, Foreground)>) {
        let mut signals = Vec::new();
        {
            let mut state = self.state.lock();
            let mut echo = Vec::new();
            for &byte in src {
                if let Some(signal) = state.ldisc.input(byte, &mut echo) {
                    signals.push((signal, state.ldisc.foreground()));
                }
            }
            let room = OUTPUT_CAPACITY.saturating_sub(state.output.len());
            state.output.extend(echo.into_iter().take(room));
        }
        self.wake_all();
        (src.len(), signals)
    }

    /// The program's output, for the emulator.
    pub fn master_read(&self, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        loop {
            {
                let mut state = self.state.lock();
                if !state.output.is_empty() {
                    let n = dst.len().min(state.output.len());
                    for slot in dst.iter_mut().take(n) {
                        *slot = state.output.pop_front().unwrap_or(0);
                    }
                    drop(state);
                    self.output_wq.notify_all();
                    crate::task::notify_poll_key(self as *const Self as u64);
                    return Ok(n);
                }
                if self.slave_gone() {
                    return Err(Error::HungUp);
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            if self.output_wq.wait(crate::task::current(), None) == WakeReason::Interrupted {
                return Err(Error::Interrupted);
            }
        }
    }

    /// A program's read from the slave: a line (canonical), or what is queued
    /// (raw; `VMIN` 0 returns at once, `VTIME` bounds the wait in tenths of a
    /// second). End-of-file once the master is gone.
    pub fn slave_read(&self, dst: &mut [u8], nonblock: bool) -> Result<usize, Error> {
        if dst.is_empty() {
            return Ok(0);
        }
        let mut deadline = None;
        loop {
            {
                let mut state = self.state.lock();
                let t = state.ldisc.termios;
                if state.ldisc.readable() {
                    let n = state.ldisc.read(dst);
                    drop(state);
                    crate::task::notify_poll_key(self as *const Self as u64);
                    return Ok(n);
                }
                if !self.master_open() {
                    return Ok(0);
                }
                if !t.canonical() && t.cc[super::termios::VMIN] == 0 {
                    let vtime = u64::from(t.cc[super::termios::VTIME]);
                    if vtime == 0 {
                        return Ok(0);
                    }
                    let due = *deadline.get_or_insert(crate::task::ticks() + vtime * 10);
                    if crate::task::ticks() >= due {
                        return Ok(0);
                    }
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            match self.input_wq.wait(crate::task::current(), deadline) {
                WakeReason::Interrupted => return Err(Error::Interrupted),
                WakeReason::Woken | WakeReason::TimedOut => {}
            }
        }
    }

    /// A program's output: post-processed and queued for the master. Blocks
    /// (or `WouldBlock`s) while the queue is full; `HungUp` once the master is
    /// gone.
    pub fn slave_write(&self, src: &[u8], nonblock: bool) -> Result<usize, Error> {
        if src.is_empty() {
            return Ok(0);
        }
        loop {
            {
                let mut state = self.state.lock();
                if !self.master_open() {
                    return Err(Error::HungUp);
                }
                let room = OUTPUT_CAPACITY.saturating_sub(state.output.len());
                if room > 0 {
                    // Each input byte may become two (`\r\n`): take what fits.
                    let mut taken = 0;
                    let mut cooked = Vec::new();
                    for &byte in src {
                        let before = cooked.len();
                        state.ldisc.output(&[byte], &mut cooked);
                        if cooked.len() > room {
                            cooked.truncate(before);
                            break;
                        }
                        taken += 1;
                    }
                    if taken > 0 {
                        state.output.extend(cooked);
                        drop(state);
                        self.master_events.fetch_add(1, Ordering::AcqRel);
                        self.output_wq.notify_all();
                        crate::task::notify_poll_key(self as *const Self as u64);
                        return Ok(taken);
                    }
                }
            }
            if nonblock {
                return Err(Error::WouldBlock);
            }
            if self.output_wq.wait(crate::task::current(), None) == WakeReason::Interrupted {
                return Err(Error::Interrupted);
            }
        }
    }

    /// `poll` revents and the edge counter for one side.
    pub fn poll_gen(&self, master: bool, events: u16) -> (u16, u64) {
        use crate::ipc::pipe::{POLLHUP, POLLIN, POLLOUT};
        let state = self.state.lock();
        let mut revents = 0;
        if master {
            if events & POLLIN != 0 && !state.output.is_empty() {
                revents |= POLLIN;
            }
            if events & POLLOUT != 0 {
                revents |= POLLOUT;
            }
            if self.slave_gone() {
                revents |= POLLHUP;
            }
            (revents, self.master_events.load(Ordering::Acquire))
        } else {
            if events & POLLIN != 0 && state.ldisc.readable() {
                revents |= POLLIN;
            }
            if events & POLLOUT != 0 && state.output.len() < OUTPUT_CAPACITY {
                revents |= POLLOUT;
            }
            if !self.master_open() {
                revents |= POLLHUP | (events & POLLIN);
            }
            (revents, self.slave_events.load(Ordering::Acquire))
        }
    }

    /// Bytes a read on `master`'s side could take now (`FIONREAD`).
    pub fn queued(&self, master: bool) -> usize {
        let state = self.state.lock();
        if master {
            state.output.len()
        } else {
            state.ldisc.available()
        }
    }
}
