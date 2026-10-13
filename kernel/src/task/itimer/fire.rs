//! Expiry: collecting the due timers under the table lock, deciding and
//! sending their signals after it (see the module docs of [`super`]).
//!
//! An expiry goes through three steps: the table lock pops the due timers and
//! re-arms the repeating ones ([`Table::fired`]); with no table lock the
//! target's signal state says whether the signal is worth sending
//! ([`signal::timer_gate`]: not when it is already pending or ignored); and
//! one more table pass books the outcome (the timer's overrun count).

use core::sync::atomic::Ordering;

use super::{next_deadline, with_table, Kind, Notify, Table, CPU_ARMED, FIRED, INTERVAL_POOL};
use crate::task::signal::{self, TimerGate};
use crate::task::{TaskState, MAX_TASKS, TASKS};

/// A signal due from a timer, collected under the lock and sent after it.
#[derive(Clone, Copy)]
struct Fire {
    /// The timer's slot, and what identifies it there (a slot may be freed
    /// and reused while the signal is decided).
    index: usize,
    pml4: u64,
    kind: Kind,
    id: i32,
    owner: usize,
    notify: Notify,
    signo: u8,
    value: u64,
    /// Periods that passed unserved before this expiry.
    missed: u32,
}

/// Most expirations collected per pass; a pass repeats while more are due.
const BATCH: usize = 8;

impl Table {
    /// Record timer `index` as fired at `now_ns`, re-arm it if it repeats,
    /// and describe the expiry.
    fn fired(&mut self, index: usize, now_ns: u64) -> Fire {
        let timer = &mut self.timers[index];
        let mut missed = 0u64;
        if timer.armed && !timer.kind.is_cpu() {
            if timer.interval_ns > 0 {
                // Skip the periods that already passed while nothing ran:
                // they are the overrun, not a burst of signals.
                missed = now_ns.saturating_sub(timer.deadline_ns) / timer.interval_ns;
                let next = timer
                    .deadline_ns
                    .saturating_add(timer.interval_ns.saturating_mul(missed + 1));
                timer.deadline_ns = next;
                self.queue.arm(index, next);
            } else {
                timer.armed = false;
            }
        }
        self.describe(index, missed)
    }

    fn describe(&self, index: usize, missed: u64) -> Fire {
        let timer = &self.timers[index];
        Fire {
            index,
            pml4: timer.pml4,
            kind: timer.kind,
            id: timer.id,
            owner: timer.owner,
            notify: timer.notify,
            signo: timer.signo,
            value: timer.value,
            missed: missed.min(i32::MAX as u64) as u32,
        }
    }

    /// Book what happened to the signal of `fire`: sent (it carried the
    /// periods it missed) or suppressed (one more expiry without a signal).
    fn book(&mut self, fire: &Fire, sent: bool) {
        let Some(timer) = self.timers.get_mut(fire.index) else {
            return;
        };
        if !(timer.used
            && timer.pml4 == fire.pml4
            && timer.kind == fire.kind
            && timer.id == fire.id)
        {
            return;
        }
        if sent {
            timer.overrun_last = fire.missed;
            timer.overrun = 0;
        } else {
            timer.overrun = timer
                .overrun
                .saturating_add(fire.missed)
                .saturating_add(1)
                .min(i32::MAX as u32);
        }
    }
}

/// Fire every wall-clock timer due at `now_ns`. Call from a scheduler entry
/// or the deadline interrupt with interrupts off and no lock held.
pub fn fire_due(now_ns: u64) {
    match next_deadline() {
        Some(deadline) if deadline <= now_ns => {}
        _ => return,
    }
    loop {
        let mut batch = [None; BATCH];
        let mut count = 0;
        with_table(|table| {
            while count < BATCH {
                let Some((_, index)) = table.queue.pop_due(now_ns) else {
                    break;
                };
                if table.timers[index].used && table.timers[index].armed {
                    batch[count] = Some(table.fired(index, now_ns));
                    count += 1;
                }
            }
        });
        deliver(&batch);
        if count < BATCH {
            return;
        }
    }
}

/// [`fire_due`] at the current time. The clock is read only when a timer is
/// armed at all, so the scheduler's hot path pays one atomic load.
pub fn fire_due_now() {
    if next_deadline().is_some() {
        fire_due(crate::arch::clock::monotonic_ns());
    }
}

/// Book `ns` of CPU time to process `pml4`'s CPU-time timers: `user` says
/// the time was spent in ring 3 (`ITIMER_VIRTUAL` only counts that).
pub fn charge_cpu(pml4: u64, ns: u64, user: bool) {
    if CPU_ARMED.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut batch = [None; BATCH];
    let mut count = 0;
    with_table(|table| {
        for index in 0..INTERVAL_POOL {
            if count == BATCH {
                break;
            }
            let timer = &mut table.timers[index];
            if !(timer.used && timer.armed && timer.pml4 == pml4 && timer.kind.is_cpu()) {
                continue;
            }
            if timer.kind == Kind::Virtual && !user {
                continue;
            }
            if timer.remaining_ns > ns {
                timer.remaining_ns -= ns;
                continue;
            }
            // Expired: reload from the interval (the overshoot is dropped,
            // as Linux's tick-granular accounting does) or disarm.
            if timer.interval_ns > 0 {
                timer.remaining_ns = timer.interval_ns;
            } else {
                timer.armed = false;
                timer.remaining_ns = 0;
                table.cpu_armed -= 1;
            }
            batch[count] = Some(table.describe(index, 0));
            count += 1;
        }
    });
    deliver(&batch);
}

/// The live thread of process `pml4` a process-directed timer signal is
/// aimed at: the timer's owner if it still runs, else any other thread.
fn live_thread(pml4: u64, owner: usize) -> Option<usize> {
    let tasks = TASKS.lock();
    let live = |slot: usize| {
        slot != 0
            && tasks[slot]
                .as_ref()
                .is_some_and(|task| task.pml4 == pml4 && task.state != TaskState::Done)
    };
    if owner < MAX_TASKS && live(owner) {
        return Some(owner);
    }
    (1..MAX_TASKS).find(|&slot| live(slot))
}

/// What to do with `fire`: the thread to signal and whether the signal is
/// thread-directed, or `None` (no live target, or `SIGEV_NONE`).
fn target_of(fire: &Fire) -> Option<(usize, bool)> {
    match fire.notify {
        Notify::None => None,
        Notify::Process => live_thread(fire.pml4, fire.owner).map(|slot| (slot, false)),
        Notify::Thread { slot, generation } => live_thread(fire.pml4, slot)
            .filter(|&live| live == slot && signal::thread_generation(slot) == generation)
            .map(|slot| (slot, true)),
    }
}

/// Decide each collected expiry, send the signals that are worth sending,
/// and book the outcomes in one table pass.
fn deliver(batch: &[Option<Fire>; BATCH]) {
    let mut sent = [false; BATCH];
    for (slot, fire) in batch.iter().enumerate() {
        let Some(fire) = fire else {
            continue;
        };
        let Some((target, thread_directed)) = target_of(fire) else {
            continue;
        };
        match signal::timer_gate(fire.pml4, target, fire.signo, thread_directed) {
            TimerGate::Send => {
                let info = match fire.kind {
                    Kind::Posix => {
                        signal::SigInfo::timer(fire.id as usize, fire.missed, fire.value)
                    }
                    _ => signal::SigInfo::kernel(),
                };
                FIRED.fetch_add(1, Ordering::Relaxed);
                signal::send_timer(target, fire.signo, info, thread_directed);
                sent[slot] = true;
            }
            // Pending: the expiry only counts. Ignored: it counts too (a
            // handler installed later still sees the overrun).
            TimerGate::Pending | TimerGate::Ignored => {}
        }
    }
    if batch.iter().all(Option::is_none) {
        return;
    }
    with_table(|table| {
        for (slot, fire) in batch.iter().enumerate() {
            if let Some(fire) = fire {
                if fire.notify != Notify::None {
                    table.book(fire, sent[slot]);
                }
            }
        }
    });
}
