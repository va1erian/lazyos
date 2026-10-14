//! A write-back that waits for another task's write-back in flight stops
//! waiting when the waiter is being killed (issue #858): the kernel loop
//! would otherwise keep a killed process alive.

use super::*;
use crate::fs::openfile::{OpenFile, BEFORE_FLUSH};
use crate::task::signal::{self, SigInfo};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

static WAITER: AtomicUsize = AtomicUsize::new(0);
static FILE: spin::Mutex<Option<Arc<OpenFile>>> = spin::Mutex::new(None);
static OUTCOME: spin::Mutex<Option<(bool, u64)>> = spin::Mutex::new(None);

/// Runs between another task's write-back taking the page and writing it:
/// `WAITER` asks for a waiting write-back of the same file, which can only
/// wait for the page in flight. Records whether it succeeded and how long it
/// took (timer ticks).
fn wait_for_the_flush_in_flight() {
    *BEFORE_FLUSH.lock() = None;
    let Some(file) = FILE.lock().take() else {
        return;
    };
    let me = task::current();
    task::harness::switch_current(WAITER.load(Ordering::Relaxed));
    let began = task::ticks();
    let ok = file.writeback(0, 0).is_ok();
    let took = task::ticks() - began;
    task::harness::switch_current(me);
    *OUTCOME.lock() = Some((ok, took));
}

/// A waiting write-back of a file whose page is being written by someone
/// else returns at once when its caller was sent `SIGKILL` (instead of
/// yielding until its spin limit and failing with `EIO`).
pub fn killed_writeback_stops_waiting() -> Result<(), String> {
    let data = Data::new(0)?;
    let path = "/data/killedwb";
    put(path, &pattern(PAGE as usize, 5))?;
    let space = Space::new()?;
    space.enter();
    let fd = open_rw(path)?;
    check!(
        mmap(BASE, PAGE, RW, MAP_SHARED | MAP_FIXED, fd, 0) == BASE,
        "mmap"
    );
    space.store(BASE + 8, b"dirty")?;
    let file = match task::fd_clone(fd as usize) {
        Some(task::Fd::Vfs { ref file }) => Arc::clone(file),
        _ => return Err(format!("descriptor {fd} is not a VFS file")),
    };

    // The waiter: a task inside the kernel that has been sent SIGKILL.
    let victim = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    task::harness::set_kind(victim, task::Kind::Native);
    task::harness::set_kernel_frame(victim).ok_or("no frame")?;
    signal::send_to_slot(
        task::KERNEL_TASK,
        victim,
        signal::SIGKILL,
        SigInfo::kernel(),
    )
    .map_err(|error| format!("kill: {error:?}"))?;
    WAITER.store(victim, Ordering::Relaxed);
    *FILE.lock() = Some(file);
    *OUTCOME.lock() = None;
    *BEFORE_FLUSH.lock() = Some(wait_for_the_flush_in_flight);
    crate::fs::openfile::writeback_all(true);
    let outcome = OUTCOME.lock().take();
    *BEFORE_FLUSH.lock() = None;
    check!(
        matches!(outcome, Some((true, _))),
        "a killed task's waiting write-back did not return cleanly: {outcome:?}"
    );

    task::harness::finish(victim, 0);
    while task::reap_child().is_some() {}
    check!(munmap(BASE, PAGE) == 0, "munmap");
    close(fd);
    drop(space);
    data.check_clean()
}
