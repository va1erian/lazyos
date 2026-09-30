//! Per-task ring of recent syscalls and signal deliveries (issue #375).
//!
//! When a ring-3 task dies of a wild jump, the fault frame alone says where
//! it landed, not how it got there. This ring records, per slot, the last few
//! Linux syscalls (number, first argument, result) and every handler frame the
//! kernel wrote for the task (signal, delivery path, the interrupted `rip` and
//! `rsp`), so the fatal-fault report can print the steps that led to the
//! fault from a CI serial log alone.
//!
//! The ring never allocates: it is written from the syscall gate and from the
//! timer sweep (IRQ context, task table locked), where the heap may be held by
//! the interrupted task. Each `record` takes its own spin lock for a few
//! stores with interrupts off, so an IRQ can never find the lock held.

use spin::Mutex;

use super::MAX_TASKS;

/// Events kept per task. Twelve covers a shell's fork/exec/wait cycle with
/// room for the signal that interrupted it.
pub const RING_LEN: usize = 12;

/// Which boundary wrote a handler frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `deliver_linux`, on the way out of a syscall.
    SyscallReturn,
    /// The scheduler, for a task preempted in user mode.
    TimerSweep,
    /// A synchronous CPU fault turned into a signal; carries the `siginfo`
    /// address (the faulting address for `SIGSEGV`/`SIGBUS`, else the `rip`).
    Fault { addr: u64 },
}

impl Via {
    /// Short label for the report.
    pub const fn label(self) -> &'static str {
        match self {
            Via::SyscallReturn => "syscall return",
            Via::TimerSweep => "timer sweep",
            Via::Fault { .. } => "fault",
        }
    }
}

/// One recorded step of a task's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// Empty ring slot.
    None,
    /// A Linux syscall completed with `result`.
    Syscall { nr: u64, a1: u64, result: u64 },
    /// A handler frame for `sig` was written over the context `rip`/`rsp`.
    Signal {
        sig: u8,
        via: Via,
        rip: u64,
        rsp: u64,
    },
}

#[derive(Clone, Copy)]
struct Ring {
    events: [Event; RING_LEN],
    /// Index of the next write; the oldest event sits there too.
    next: usize,
}

impl Ring {
    const EMPTY: Ring = Ring {
        events: [Event::None; RING_LEN],
        next: 0,
    };

    fn push(&mut self, event: Event) {
        self.events[self.next] = event;
        self.next = (self.next + 1) % RING_LEN;
    }

    /// The events oldest first, `Event::None` for never-written slots.
    fn ordered(&self) -> [Event; RING_LEN] {
        let mut out = [Event::None; RING_LEN];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = self.events[(self.next + i) % RING_LEN];
        }
        out
    }
}

static RINGS: Mutex<[Ring; MAX_TASKS]> = Mutex::new([Ring::EMPTY; MAX_TASKS]);

fn with_ring<R>(slot: usize, f: impl FnOnce(&mut Ring) -> R) -> Option<R> {
    if slot >= MAX_TASKS {
        return None;
    }
    // Interrupts off around the lock: the timer sweep records too, and a tick
    // landing while a syscall holds this lock would spin forever.
    x86_64::instructions::interrupts::without_interrupts(|| Some(f(&mut RINGS.lock()[slot])))
}

/// Record a completed Linux syscall of `slot`.
pub fn record_syscall(slot: usize, nr: u64, a1: u64, result: u64) {
    with_ring(slot, |ring| ring.push(Event::Syscall { nr, a1, result }));
}

/// Record a handler frame written for `slot`.
pub fn record_signal(slot: usize, sig: u8, via: Via, rip: u64, rsp: u64) {
    with_ring(slot, |ring| ring.push(Event::Signal { sig, via, rip, rsp }));
}

/// Forget a slot's history: called when the slot is given to a new task, so
/// a report never shows a previous occupant's steps.
pub fn clear(slot: usize) {
    with_ring(slot, |ring| *ring = Ring::EMPTY);
}

/// The recorded history of `slot`, oldest first.
pub fn history(slot: usize) -> [Event; RING_LEN] {
    with_ring(slot, |ring| ring.ordered()).unwrap_or([Event::None; RING_LEN])
}
