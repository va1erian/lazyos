//! Frame shapes for the kernel suite (`SCHED:STUCK`, issue #861): tasks
//! that were switched out the way an interrupt stub or a syscall leaves them.

use super::harness::frame_of;
use super::TASKS;

/// Make `slot` look like a task an interrupt switched out *inside the
/// kernel* (an IRQ stub's `preempt_point`) while it was running user code:
/// its saved frame is a kernel frame, a copy of the old one placed lower on
/// its stack, and the frame the CPU pushed at the top of the stack stays the
/// ring-3 one with `user_rip`. Returns whether the slot holds a task.
pub fn set_preempted_in_irq(slot: usize, user_rip: u64) -> bool {
    use x86_64::instructions::segmentation::{Segment, CS};
    let Some(frame) = frame_of(slot) else {
        return false;
    };
    let nested = frame - 20 * 8 - 64;
    let kernel_cs = u64::from(CS::get_reg().0);
    // SAFETY: `frame` is the 20-word frame `spawn_fork` built at the top of
    // the slot's own kernel stack, and the 20 words and padding below it are
    // the same stack's unused space.
    unsafe {
        for word in 0..20 {
            let value = super::sys::frame_word(frame, word);
            super::sys::put_frame_word(nested, word, value);
        }
        super::sys::put_frame_word(nested, super::signal::FRAME_RIP_INDEX + 1, kernel_cs);
        super::sys::put_frame_word(frame, super::signal::FRAME_RIP_INDEX, user_rip);
    }
    if let Some(task) = TASKS.lock()[slot].as_mut() {
        task.rsp = nested;
    }
    true
}

/// Put `rip` where a Linux `syscall` entry leaves the user `rip` on the
/// kernel stack of a task that has not run since it was spawned (the top of
/// its stack, `arch::linux`), as a task parked inside a syscall has it.
pub fn set_syscall_user_rip(slot: usize, rip: u64) -> bool {
    let Some(top) = TASKS.lock()[slot].as_ref().map(|task| task.kstack_top) else {
        return false;
    };
    // SAFETY: the top 40 bytes of a spawned task's kernel stack hold its
    // first frame; nothing reads them until the task runs.
    unsafe { core::ptr::write_volatile((top - 24) as *mut u64, rip) };
    true
}
