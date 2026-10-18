//! A signal handler frame whose stack page is an unread page of a file
//! mapping (audit H13). The timer sweep writes handler frames from the timer
//! interrupt, with the task table locked: reading the file there would need
//! the filesystem's lock, whose holder cannot run, and would park inside an
//! interrupt. The sweep leaves such a signal pending (a syscall return
//! delivers it in task context, where the fault may park) and gives up after
//! a bounded time.

use super::*;
use crate::task::signal::{self, Disposition, SigInfo};
use crate::task::TaskState;
use core::mem::ManuallyDrop;

const HANDLER: u64 = 0x0040_1000;
const RESTORER: u64 = 0x0040_2000;
const PREEMPTED_RIP: u64 = 0x0040_0500;

/// A process whose stack is the second page of a shared file mapping, with
/// `SIGUSR1` caught and pending; its table is installed.
struct Process {
    task: usize,
    /// Borrows the task's table for the `Space` helpers; the task owns it.
    space: ManuallyDrop<Space>,
    fd: u64,
}

impl Process {
    fn new(path: &str) -> Result<Process, String> {
        put(path, &pattern(2 * PAGE as usize, 7))?;
        let task = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        let table = PhysAddr::new(task::harness::pml4(task).ok_or("no address space")?);
        let space = ManuallyDrop::new(Space {
            table,
            boot: mem::kernel_table(),
        });
        space.enter();
        let fd = open_rw(path)?;
        check!(
            mmap(BASE, 2 * PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
            "mmap"
        );
        for page in 0..2 {
            check!(
                space.entry(BASE + page * PAGE).is_none(),
                "page {page} was mapped eagerly"
            );
        }
        check!(
            task::harness::set_user_frame(task, PREEMPTED_RIP, BASE + 2 * PAGE - 0x100),
            "no frame"
        );
        let handler = Disposition::Handler {
            handler: HANDLER,
            flags: 0,
            restorer: RESTORER,
            mask: 0,
        };
        signal::set_action(task, signal::SIGUSR1, handler)
            .map_err(|error| format!("sigaction: {error:?}"))?;
        signal::send_to_slot(
            task::KERNEL_TASK,
            task,
            signal::SIGUSR1,
            SigInfo::user(task::KERNEL_TASK, signal::SI_USER),
        )
        .map_err(|error| format!("send: {error:?}"))?;
        Ok(Process { task, space, fd })
    }

    /// One timer sweep with this process's table installed and user pointers
    /// checked, as when the scheduler interrupts it.
    fn sweep(&self) {
        self.space.enter();
        let trust = crate::user_ptr::set_trust_kernel_pointers(false);
        task::harness::run_sweep();
        crate::user_ptr::set_trust_kernel_pointers(trust);
    }

    fn entered(&self) -> bool {
        task::harness::frame_regs(self.task).is_some_and(|regs| regs.rip == HANDLER)
    }
}

pub fn handler_frame_never_reads_a_file_in_the_sweep() -> Result<(), String> {
    on_both(frame_case)
}

/// The sweep defers a handler whose frame needs a file page, reads nothing,
/// delivers it once the pages are resident, and ends a task whose frame never
/// becomes writable after [`signal::FRAME_DEFER_LIMIT`] sweeps.
fn frame_case(dir: &str) -> Result<(), String> {
    signal::harness::reset();
    let boot = mem::kernel_table();
    let first = Process::new(&format!("{dir}/h13"))?;

    // The interrupt must not read the file.
    let epoch = mem::filemap::fault_epoch();
    first.sweep();
    check!(
        mem::filemap::fault_epoch() == epoch,
        "the timer sweep read a file page"
    );
    check!(
        !first.entered(),
        "a handler frame was written onto an unread file page"
    );
    check!(
        signal::pending(first.task) & (1 << signal::SIGUSR1) != 0,
        "the deferred signal is no longer pending"
    );
    for page in 0..2 {
        check!(
            first.space.entry(BASE + page * PAGE).is_none(),
            "the sweep mapped page {page}"
        );
    }

    // Task context (a syscall return) reads the pages in; the next sweep
    // then delivers.
    for page in 0..2 {
        first.space.touch(BASE + page * PAGE, true)?;
    }
    first.sweep();
    check!(
        first.entered(),
        "the handler was not entered once the stack was resident"
    );

    // A task that never leaves user mode and whose stack never becomes
    // resident is ended after the limit, not left hanging.
    let second = Process::new(&format!("{dir}/h13b"))?;
    for round in 0..signal::FRAME_DEFER_LIMIT {
        second.sweep();
        check!(
            task::harness::state(second.task) != Some(TaskState::Done),
            "round {round}: ended before the limit"
        );
    }
    second.sweep();
    check!(
        task::harness::state(second.task) == Some(TaskState::Done),
        "a frame that never becomes resident is still pending"
    );

    // Back on the boot table: release both processes.
    mem::switch_to(boot);
    close(first.fd);
    close(second.fd);
    task::harness::finish(first.task, 0);
    task::harness::finish(second.task, 0);
    while task::reap_child().is_some() {}
    signal::harness::reset();
    Ok(())
}
