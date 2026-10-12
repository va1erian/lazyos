//! Ring-3 exceptions are contained to the faulting process (issue #7): a bad
//! pointer, privileged instruction or divide error ends that process with a
//! `128 + signal` status and never takes the machine (or its siblings) down.

use super::*;
use crate::arch::fault::{self, Fault};
use crate::arch::fault_report;
use crate::mem::vma::{Kind, Prot};
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

const HANDLER: u64 = 0x0040_2000;
const RESTORER: u64 = 0x0040_3000;

/// A synthetic saved-register frame for `exception` on a fresh user stack:
/// 15 registers, an error code when the class has one, then RIP, CS, RFLAGS,
/// RSP and SS. Returns the frame and the stack that must outlive it.
fn exception_frame(exception: signal::Exception) -> Result<(Vec<u64>, UserStack), String> {
    let stack = UserStack::new()?;
    let top = stack.top();
    let rip = exception.rip_index();
    let mut frame = alloc::vec![0u64; rip + 5];
    frame[9] = 0xd1d1; // rdi
    frame[rip] = 0x0040_1234;
    frame[rip + 1] = 0x23;
    frame[rip + 2] = 0x202;
    frame[rip + 3] = top - 0x100;
    frame[rip + 4] = 0x1b;
    Ok((frame, stack))
}

fn install_handler(sig: u8) -> Result<(), String> {
    signal::set_action(
        task::current(),
        sig,
        signal::Disposition::Handler {
            handler: HANDLER,
            flags: 0,
            restorer: RESTORER,
            mask: 0,
        },
    )
    .map_err(|e| format!("set_action({sig}): {e:?}"))
}

const EXCEPTIONS: [signal::Exception; 3] = [
    signal::Exception::DivideError,
    signal::Exception::InvalidOpcode,
    signal::Exception::GeneralProtection,
];

/// #DE/#UD/#GP enter a registered handler with the right signal number, and
/// the resumed frame is a user frame on the handler's stack (issue #246).
pub fn exception_enters_handler() -> Result<(), String> {
    for exception in EXCEPTIONS {
        fresh()?;
        let sig = exception.signal();
        install_handler(sig)?;
        let (mut frame, stack) = exception_frame(exception)?;
        let rip = exception.rip_index();
        let old_rsp = frame[rip + 3];
        check!(
            signal::deliver_exception(frame.as_mut_ptr() as u64, exception),
            "{exception:?} with a handler was not delivered"
        );
        check!(frame[rip] == HANDLER, "rip is {:#x}", frame[rip]);
        check!(frame[9] == sig as u64, "rdi is {:#x}, not {sig}", frame[9]);
        check!(
            frame[rip + 3] < old_rsp && frame[rip + 3] > stack.base(),
            "handler rsp {:#x} is outside the user stack",
            frame[rip + 3]
        );
        check!(
            frame[rip + 1] == 0x23,
            "cs was rewritten: {:#x}",
            frame[rip + 1]
        );
        check!(
            signal::blocked(task::current()) & (1 << sig) != 0,
            "{sig} is not blocked while its handler runs"
        );
    }
    Ok(())
}

/// Without a deliverable handler the fault is not consumed: default, ignore,
/// and a signal blocked by an earlier fault (which would loop) all fall
/// through to the kill path and leave the frame untouched.
pub fn exception_without_handler_falls_through() -> Result<(), String> {
    for exception in EXCEPTIONS {
        fresh()?;
        let sig = exception.signal();
        let (mut frame, _stack) = exception_frame(exception)?;
        let before = frame.clone();
        let at = frame.as_mut_ptr() as u64;
        check!(
            !signal::deliver_exception(at, exception),
            "{exception:?} delivered with the default action"
        );
        signal::set_action(task::current(), sig, signal::Disposition::Ignore)
            .map_err(|e| format!("ignore {sig}: {e:?}"))?;
        check!(
            !signal::deliver_exception(at, exception),
            "{exception:?} delivered while ignored"
        );
        install_handler(sig)?;
        check!(signal::deliver_exception(at, exception), "first delivery");
        let entered = frame.clone();
        frame.copy_from_slice(&before);
        check!(
            !signal::deliver_exception(at, exception),
            "{exception:?} re-delivered while blocked: a handler that faults would loop"
        );
        check!(frame == before, "a refused delivery rewrote the frame");
        check!(entered != before, "the first delivery changed nothing");
    }
    Ok(())
}

/// `si_addr` is filled in for `SIGILL`/`SIGFPE` faults, not just `SIGSEGV`.
pub fn fault_siginfo_carries_address() -> Result<(), String> {
    fresh()?;
    let stack = UserStack::new()?;
    let top = stack.top();
    let regs = signal::UserRegs {
        rsp: top - 0x100,
        rip: 0x0040_1000,
        rflags: 0x202,
        ..Default::default()
    };
    for (sig, code) in [
        (signal::SIGILL, signal::ILL_ILLOPC),
        (signal::SIGFPE, signal::FPE_INTDIV),
    ] {
        let info = signal::SigInfo::fault(code, 0x0040_1000);
        let result = signal::build_linux_frame(
            top,
            &regs,
            sig,
            HANDLER,
            signal::SA_SIGINFO,
            RESTORER,
            0,
            0,
            &info,
        )
        .ok_or("frame does not fit")?;
        // Safety: `build_linux_frame` just wrote this siginfo on the stack.
        let addr = unsafe { core::ptr::read_volatile((result.info + 16) as *const u64) };
        check!(addr == 0x0040_1000, "si_addr for signal {sig} is {addr:#x}");
    }
    Ok(())
}

/// Soak: thousands of deliveries, each on a fresh frame, all enter the handler
/// and none leaks blocked-mask or action state between rounds.
pub fn soak_exception_delivery() -> Result<(), String> {
    fresh()?;
    for round in 0..3000usize {
        let exception = EXCEPTIONS[round % EXCEPTIONS.len()];
        let sig = exception.signal();
        install_handler(sig)?;
        let (mut frame, _stack) = exception_frame(exception)?;
        check!(
            signal::deliver_exception(frame.as_mut_ptr() as u64, exception),
            "round {round}: {exception:?} was not delivered"
        );
        check!(
            frame[exception.rip_index()] == HANDLER,
            "round {round}: rip"
        );
        signal::set_blocked(task::current(), 0);
    }
    signal::harness::reset();
    Ok(())
}

/// The `code:` line is all `??` for `rip`, and building it allocated no frame.
fn code_unreadable(rip: u64) -> Result<(), String> {
    let free = mem::frame_stats().free;
    let line = fault_report::code_line(rip);
    let want = format!("user: code at {rip:#x}:{}", " ??".repeat(16));
    check!(line == want, "rip {rip:#x}: {line:?}, want {want:?}");
    check!(
        mem::frame_stats().free == free,
        "reporting rip {rip:#x} allocated a frame"
    );
    Ok(())
}

fn report_code_cases() -> Result<(), String> {
    const PAGE: u64 = 4096;
    let table = mem::kernel_table();
    // Kernel addresses (text, heap, the physical-memory window): mapped and
    // readable in this very table, but not user memory.
    let boxed = alloc::boxed::Box::new([0x90u8; 32]);
    let kernel = [
        report_code_cases as usize as u64,
        &*boxed as *const [u8; 32] as u64,
        mem::phys_to_virt(PhysAddr::new(0x1000)).as_u64(),
        0xffff_8000_0000_0000,
        0xffff_ffff_ffff_f000,
    ];
    for rip in kernel {
        code_unreadable(rip)?;
    }
    // Non-canonical, and ranges that run off the end of the address space.
    for rip in [
        0x0000_8000_0000_0000,
        0x0001_0000_0000_0000,
        0x7fff_8000_0000_0000,
        0x8000_0000_0000_0000,
        0xdead_beef_dead_beef,
        u64::MAX - 5,
        u64::MAX,
    ] {
        code_unreadable(rip)?;
    }
    // Unmapped user addresses, including the last bytes below the limit (the
    // 16 bytes cross it).
    for rip in [0, 1, 0x7000_dead_0000, 0x0000_7fff_ffff_fff8] {
        code_unreadable(rip)?;
    }

    // A page a fault would demand-materialize is left alone, and it is the
    // one the demand-paging reader would have materialized.
    // Where the Linux shim maps user memory (the low page-table entries are
    // the kernel's own, without the user bit).
    let base = crate::process::linux::MMAP_BASE + 0x4000_0000;
    mem::vma::insert(
        table,
        base,
        base + 2 * PAGE,
        Prot::READ | Prot::WRITE,
        Kind::Anon,
    );
    let outcome = (|| {
        code_unreadable(base)?;
        code_unreadable(base + PAGE + 7)?;
        check!(
            raw_entry(table, base).is_none() && raw_entry(table, base + PAGE).is_none(),
            "reporting materialized a page"
        );
        check!(
            crate::user_ptr::try_read::<u8>(base).is_ok(),
            "the control read did not demand-page"
        );
        let frame = frame_of(table, base)?;
        let bytes: [u8; 16] = core::array::from_fn(|i| 0x0f + i as u8);
        let ptr = mem::phys_to_virt(PhysAddr::new(frame)).as_mut_ptr::<u8>();
        for (offset, byte) in bytes.iter().enumerate() {
            // SAFETY: the frame is mapped through the physical-memory window.
            unsafe {
                ptr.add(offset).write_volatile(*byte);
                ptr.add(0xff8 + offset % 8).write_volatile(*byte);
            }
        }
        // All 16 bytes of a present page...
        let line = fault_report::code_line(base);
        let hex: alloc::string::String = bytes.iter().map(|b| format!(" {b:02x}")).collect();
        check!(
            line == format!("user: code at {base:#x}:{hex}"),
            "present page: {line:?}"
        );
        // ... and the bytes before a page that is not there.
        let line = fault_report::code_line(base + 0xff8);
        let head: alloc::string::String =
            (0..8).map(|i| format!(" {:02x}", bytes[8 + i])).collect();
        check!(
            line == format!(
                "user: code at {:#x}:{head}{}",
                base + 0xff8,
                " ??".repeat(8)
            ),
            "page edge: {line:?}"
        );
        check!(
            raw_entry(table, base + PAGE).is_none(),
            "the next page was materialized"
        );
        Ok(())
    })();
    mem::unmap_range(table, base, base + 2 * PAGE);
    mem::vma::remove(table, base, base + 2 * PAGE);
    outcome
}

/// The fault report prints the bytes at a wild `rip` without faulting,
/// allocating or paging anything in: kernel addresses, non-canonical ones and
/// unmapped ones print `??`; a page that is present prints (the part of a
/// range that crosses into one that is not too).
pub fn report_code_never_faults() -> Result<(), String> {
    fresh()?;
    let trusted = crate::user_ptr::set_trust_kernel_pointers(false);
    let outcome = report_code_cases();
    crate::user_ptr::set_trust_kernel_pointers(trusted);
    outcome
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("fault_exception_enters_handler", exception_enters_handler),
    (
        "fault_exception_without_handler_falls_through",
        exception_without_handler_falls_through,
    ),
    (
        "fault_siginfo_carries_address",
        fault_siginfo_carries_address,
    ),
    ("fault_soak_exception_delivery", soak_exception_delivery),
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
    ("fault_report_code_never_faults", report_code_never_faults),
];
