//! Ring-3 exceptions are contained to the faulting process (issue #7): a bad
//! pointer, privileged instruction or divide error ends that process with a
//! `128 + signal` status and never takes the machine (or its siblings) down.

use super::*;
use crate::arch::fault::{self, Fault};
use crate::task::signal;
use crate::task::TaskState;

const ALL_FAULTS: [Fault; 10] = [
    Fault::DivideError,
    Fault::InvalidOpcode,
    Fault::DeviceNotAvailable,
    Fault::GeneralProtection,
    Fault::StackSegment,
    Fault::SegmentNotPresent,
    Fault::InvalidTss,
    Fault::FloatingPoint,
    Fault::AlignmentCheck,
    Fault::BadAccess,
];

fn fresh() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    signal::harness::reset();
    Ok(())
}

/// Only a saved RPL-3 code segment counts as a user fault: the kernel's own
/// selectors (and anything with a lower RPL) keep the diagnostic halt.
pub fn ring_classification() -> Result<(), String> {
    let selectors = crate::arch::gdt::selectors();
    check!(
        fault::from_user(selectors.user_code as u64),
        "the user code selector {:#x} is not classed as ring 3",
        selectors.user_code
    );
    // The kernel code selector is GDT entry 1 (0x08, RPL 0).
    check!(
        !fault::from_user(0x08),
        "the kernel code selector is classed as ring 3"
    );
    for rpl in 0..3u64 {
        check!(
            !fault::from_user(0x18 | rpl),
            "RPL {rpl} was classed as ring 3"
        );
    }
    Ok(())
}

/// Each exception maps to the signal Linux raises, and every status is in the
/// `128 + signal` range a parent decodes as "killed by signal".
pub fn signal_mapping() -> Result<(), String> {
    check!(
        Fault::BadAccess.signal() == signal::SIGSEGV,
        "#PF is not SIGSEGV"
    );
    check!(
        Fault::GeneralProtection.signal() == signal::SIGSEGV,
        "#GP is not SIGSEGV"
    );
    check!(
        Fault::DivideError.signal() == signal::SIGFPE,
        "#DE is not SIGFPE"
    );
    check!(
        Fault::InvalidOpcode.signal() == signal::SIGILL,
        "#UD is not SIGILL"
    );
    check!(
        Fault::AlignmentCheck.signal() == signal::SIGBUS,
        "#AC is not SIGBUS"
    );
    for fault in ALL_FAULTS {
        check!(
            fault::exit_status(fault) == 128 + fault.signal() as u64,
            "{}: status {}",
            fault.name(),
            fault::exit_status(fault)
        );
    }
    Ok(())
}

/// The kernel task is never killed by the fault path, even if asked.
pub fn kernel_task_is_never_killed() -> Result<(), String> {
    fresh()?;
    check!(
        fault::kill_current(Fault::BadAccess) == 0,
        "the fault path terminated the kernel task"
    );
    check!(
        task::harness::state(task::KERNEL_TASK) == Some(TaskState::Runnable),
        "the kernel task is no longer runnable"
    );
    Ok(())
}

/// A faulting task dies with the fault's status, its parent gets `SIGCHLD`
/// and can reap it, and an unrelated sibling keeps running.
pub fn user_fault_kills_only_the_faulter() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(|e| format!("spawn parent: {e}"))?;
    task::harness::switch_current(parent);
    let victim = task::spawn_fork().map_err(|e| format!("spawn victim: {e}"))?;
    let bystander = task::spawn_fork().map_err(|e| format!("spawn bystander: {e}"))?;

    task::harness::switch_current(victim);
    check!(
        fault::kill_current(Fault::BadAccess) == 1,
        "the page fault did not end exactly the faulting task"
    );
    task::harness::switch_current(parent);
    check!(
        task::harness::state(victim) == Some(TaskState::Done),
        "the faulting task is still {:?}",
        task::harness::state(victim)
    );
    check!(
        task::harness::state(bystander) == Some(TaskState::Runnable),
        "a sibling was collateral damage: {:?}",
        task::harness::state(bystander)
    );
    check!(
        signal::pending(parent) & (1 << signal::SIGCHLD) != 0,
        "the parent never saw SIGCHLD"
    );
    let (slot, status) = task::reap_child().ok_or("the parent could not reap the victim")?;
    check!(
        slot == victim && status == 128 + signal::SIGSEGV as u64,
        "reaped {slot}/{status}, expected {victim}/{}",
        128 + signal::SIGSEGV as u64
    );

    task::harness::finish(bystander, 0);
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(parent, 0);
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

/// Soak: thousands of fault/reap generations across every exception class
/// leak no task slots and always report the right status.
pub fn soak_fault_generations() -> Result<(), String> {
    fresh()?;
    let parent = task::spawn_fork().map_err(|e| format!("spawn parent: {e}"))?;
    task::harness::switch_current(parent);
    for round in 0..3000usize {
        let fault = ALL_FAULTS[round % ALL_FAULTS.len()];
        let child = task::spawn_fork().map_err(|e| format!("round {round}: spawn: {e}"))?;
        task::harness::switch_current(child);
        check!(
            fault::kill_current(fault) == 1,
            "round {round}: {} did not end the task",
            fault.name()
        );
        task::harness::switch_current(parent);
        let (slot, status) =
            task::reap_child().ok_or_else(|| format!("round {round}: nothing to reap"))?;
        check!(
            slot == child && status == fault::exit_status(fault),
            "round {round}: reaped {slot}/{status} for {}",
            fault.name()
        );
    }
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(parent, 0);
    task::harness::reset();
    signal::harness::reset();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("fault_ring_classification", ring_classification),
    ("fault_signal_mapping", signal_mapping),
    (
        "fault_kernel_task_never_killed",
        kernel_task_is_never_killed,
    ),
    (
        "fault_user_fault_kills_only_faulter",
        user_fault_kills_only_the_faulter,
    ),
    ("fault_soak_generations", soak_fault_generations),
];
