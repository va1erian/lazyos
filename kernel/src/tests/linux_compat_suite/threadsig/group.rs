//! A process of threads for the per-thread signal tests: a forked leader plus
//! `clone(CLONE_VM)` threads sharing its table, every member "preempted in
//! user mode" on a mapped stack of its own.
//!
//! Delivery is the scheduler's. With the shared table installed a sweep writes
//! a handler frame for each thread that may take a signal, and a member has
//! "entered" its handler when its saved frame resumes there.

use super::*;
use crate::task::signal::{self, Disposition};
use x86_64::PhysAddr;

pub(super) const KILL: u64 = 62;
pub(super) const TKILL: u64 = 200;
pub(super) const TGKILL: u64 = 234;
pub(super) const SIGPROCMASK: u64 = 14;
pub(super) const SIGPENDING: u64 = 127;
pub(super) const SIGALTSTACK: u64 = 131;
pub(super) const GETTID: u64 = 186;

pub(super) const ESRCH: u64 = neg(3);

const HANDLER: u64 = 0x0040_1000;
const RESTORER: u64 = 0x0040_2000;
/// Where a member "was preempted"; inside no mapping, the sweep never reads code.
const PREEMPTED_RIP: u64 = 0x0040_0500;
/// First stack page; every member gets [`SPAN`] bytes of its own above it.
pub(super) const STACKS: u64 = 0x0070_0000;
pub(super) const SPAN: u64 = 0x2000;

pub(super) const USR1: u8 = signal::SIGUSR1;
pub(super) const USR2: u8 = signal::SIGUSR2;

/// A signal's bit in the kernel's internal mask order.
pub(super) const fn kbit(sig: u8) -> u64 {
    1 << sig
}

/// A signal's bit in a Linux `sigset_t`.
pub(super) const fn lbit(sig: u8) -> u64 {
    1 << (sig - 1)
}

/// One process: a leader and its threads, all in one address space.
pub(super) struct Group {
    pub(super) leader: usize,
    pub(super) threads: Vec<usize>,
    pub(super) table: PhysAddr,
}

impl Group {
    /// The leader first, then the threads.
    pub(super) fn members(&self) -> Vec<usize> {
        let mut all = vec![self.leader];
        all.extend(&self.threads);
        all
    }

    /// The stack pointer member number `index` is preempted at.
    fn rsp(index: usize) -> u64 {
        STACKS + (index as u64 + 1) * SPAN - 0x100
    }

    /// Whether `slot`'s saved frame resumes in the handler.
    pub(super) fn entered(&self, slot: usize) -> bool {
        task::harness::frame_regs(slot).is_some_and(|regs| regs.rip == HANDLER)
    }

    /// The members that entered their handler.
    pub(super) fn entered_members(&self) -> Vec<usize> {
        let all = self.members();
        all.into_iter().filter(|&slot| self.entered(slot)).collect()
    }

    /// `rt_sigreturn` for `slot`: back to where it was preempted, with the
    /// mask it had before the handler.
    pub(super) fn return_from_handler(&self, slot: usize, mask: u64) {
        let index = self.members().iter().position(|&s| s == slot).unwrap_or(0);
        task::harness::set_user_frame(slot, PREEMPTED_RIP, Self::rsp(index));
        signal::set_blocked(slot, mask);
    }

    /// Run `f` with the process's table installed and user pointers checked,
    /// as when the scheduler delivers to one of its threads.
    fn in_space<R>(&self, f: impl FnOnce() -> R) -> R {
        let kernel = mem::kernel_table();
        let trust = crate::user_ptr::set_trust_kernel_pointers(false);
        mem::switch_to(self.table);
        let result = f();
        mem::switch_to(kernel);
        crate::user_ptr::set_trust_kernel_pointers(trust);
        result
    }

    /// A tick that interrupts a member: every thread with a signal to take
    /// enters its handler.
    pub(super) fn sweep(&self) {
        self.in_space(task::harness::run_sweep);
    }

    /// The scheduler resuming `slot` into user mode: true if that ended it.
    pub(super) fn resume(&self, slot: usize) -> bool {
        self.in_space(|| task::harness::resume_delivery(slot))
    }

    /// A page fault of `slot` at `addr`: whether its handler was entered.
    pub(super) fn fault(&self, slot: usize, addr: u64) -> Result<bool, String> {
        task::harness::switch_current(slot);
        let frame = task::harness::frame_base(slot).ok_or("no frame")?;
        Ok(self.in_space(|| signal::deliver_fault(frame, signal::FRAME_RIP_INDEX, addr, 0)))
    }

    /// The thread-directed signals pending on any member.
    pub(super) fn thread_pending(&self) -> u64 {
        self.members()
            .iter()
            .fold(0, |all, &slot| all | signal::pending_thread(slot))
    }
}

/// A process of `n` threads plus its leader, `USR1` and `USR2` caught
/// process-wide, the leader the current task.
pub(super) fn group(n: usize) -> Result<Group, String> {
    fresh()?;
    signal::harness::reset();
    let leader = task::spawn_fork().map_err(|error| format!("leader: {error}"))?;
    let table = PhysAddr::new(task::harness::pml4(leader).ok_or("the leader has no table")?);
    process::map_range(table, STACKS, STACKS + (n as u64 + 2) * SPAN).map_err(to_string)?;
    task::harness::switch_current(leader);
    let mut threads = Vec::new();
    for _ in 0..n {
        threads.push(thread()?);
    }
    let group = Group {
        leader,
        threads,
        table,
    };
    for (index, slot) in group.members().into_iter().enumerate() {
        check!(
            task::harness::pml4(slot) == Some(table.as_u64()),
            "member {slot} does not share the leader's table"
        );
        check!(
            task::harness::set_user_frame(slot, PREEMPTED_RIP, Group::rsp(index)),
            "no frame for {slot}"
        );
    }
    for sig in [USR1, USR2] {
        let handler = Disposition::Handler {
            handler: HANDLER,
            flags: 0,
            restorer: RESTORER,
            mask: 0,
        };
        signal::set_action(leader, sig, handler).map_err(|e| format!("sigaction: {e:?}"))?;
    }
    Ok(group)
}

/// A new thread of the current task's process.
pub(super) fn thread() -> Result<usize, String> {
    task::spawn_thread("threadsig", process::USER_STACK_TOP, 0, 0)
        .map_err(|error| format!("thread: {error}"))
}

/// Remove finished threads the way the scheduler does once it has switched
/// away from each: the slot is freed, the shared table stays.
pub(super) fn reclaim_threads(slots: &[usize]) {
    for &slot in slots {
        task::harness::finish(slot, 0);
        task::harness::switch_current(slot);
        task::harness::simulate_tick();
        task::reclaim_pending();
    }
}

/// End the process and everything in it, releasing its address space.
pub(super) fn teardown(group: &Group) -> Result<(), String> {
    reclaim_threads(&group.threads);
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(group.leader, 0);
    task::reap_child().ok_or("the leader was not reapable")?;
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Run syscall `nr` as thread `slot`.
pub(super) fn as_thread(slot: usize, nr: u64, args: &[u64]) -> u64 {
    task::harness::switch_current(slot);
    sys(nr, args)
}

pub(super) fn tgkill(group: &Group, from: usize, tid: usize, sig: u8) -> u64 {
    as_thread(from, TGKILL, &[group.leader as u64, tid as u64, sig as u64])
}

/// `slot`'s blocked mask as `rt_sigprocmask` reports it (Linux bit order).
pub(super) fn blocked_of(slot: usize) -> u64 {
    let mut old = 0u64;
    as_thread(slot, SIGPROCMASK, &[0, 0, &mut old as *mut u64 as u64, 8]);
    old
}

pub(super) fn is_blocked(slot: usize) -> bool {
    matches!(
        task::harness::state(slot),
        Some(task::TaskState::Blocked { .. })
    )
}
