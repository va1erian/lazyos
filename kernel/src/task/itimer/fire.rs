//! Expiry: collecting the due timers under the table lock and sending their
//! signals after it (see the module docs of [`super`]).

use core::sync::atomic::Ordering;

use super::{next_deadline, with_table, Kind, Notify, Table, CPU_ARMED, FIRED, MAX_TIMERS};
use crate::task::signal;
use crate::task::{TaskState, MAX_TASKS, TASKS};

/// A signal due from a timer, collected under the lock and sent after it.
#[derive(Clone, Copy)]
struct Fire {
    pml4: u64,
    owner: usize,
    notify: Notify,
    signo: u8,
    kind: Kind,
    id: i32,
    value: u64,
    overrun: u32,
}

/// Most expirations collected per pass; a pass repeats while more are due.
const BATCH: usize = 8;

impl Table {
    /// Record timer `index` as fired at `now_ns`, re-arm it if it repeats,
    /// and describe the signal to send.
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
        let timer = &mut self.timers[index];
        timer.overrun = missed.min(i32::MAX as u64) as u32;
        Fire {
            pml4: timer.pml4,
            owner: timer.owner,
            notify: timer.notify,
            signo: timer.signo,
            kind: timer.kind,
            id: timer.id,
            value: timer.value,
            overrun: timer.overrun,
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
        for fire in batch.iter().flatten() {
            send(fire);
        }
        if count < BATCH {
            return;
        }
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
        for index in 0..MAX_TIMERS {
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
            let timer = &mut table.timers[index];
            timer.overrun = 0;
            batch[count] = Some(Fire {
                pml4,
                owner: timer.owner,
                notify: timer.notify,
                signo: timer.signo,
                kind: timer.kind,
                id: timer.id,
                value: timer.value,
                overrun: 0,
            });
            count += 1;
        }
    });
    for fire in batch.iter().flatten() {
        send(fire);
    }
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

/// Queue the signal of `fire`: through the signal layer, so a blocked
/// target is interrupted and a handler or default action runs at the
/// boundary back to ring 3.
fn send(fire: &Fire) {
    let (thread_directed, target) = match fire.notify {
        Notify::None => return,
        Notify::Process => (false, live_thread(fire.pml4, fire.owner)),
        Notify::Thread(slot) => (true, live_thread(fire.pml4, slot).filter(|&s| s == slot)),
    };
    let Some(target) = target else {
        return;
    };
    let info = match fire.kind {
        Kind::Posix => signal::SigInfo::timer(fire.id as usize, fire.overrun, fire.value),
        _ => signal::SigInfo::kernel(),
    };
    FIRED.fetch_add(1, Ordering::Relaxed);
    signal::send_timer(target, fire.signo, info, thread_directed);
}
