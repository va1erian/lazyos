//! An epoll instance shared across `fork` keeps the parent's interests when a child closes its copies.
//!
//! LazyWeb's window parks on an epoll set holding an eventfd doorbell, and then forks the engine's
//! processes; each child's `exec` closes its close-on-exec copies of both. `close` used to drop every
//! interest registered under the closed number from each epoll instance in the closer's table, and
//! the instance is the parent's own: the first child's `exec` took the doorbell out of the parent's
//! set, and the window was woken only by its timers from then on. Linux drops an interest only when
//! its open file is gone; here an interest leaves when no live task holds both the instance and the
//! file under that number.

use super::*;
use alloc::sync::Arc;

const EPOLL_CTL_ADD: u64 = 1;
const EPOLL_CTL_DEL: u64 = 2;
const POLLIN: u16 = 1;

/// `epoll_create1(0)` and `eventfd2(0, 0)` as the current task (the eventfd first, so with the
/// lower number, when `eventfd_first`), the eventfd registered for `EPOLLIN`. Returns
/// `(epoll fd, eventfd)`.
fn doorbell_set(eventfd_first: bool) -> Result<(usize, usize), String> {
    let create_eventfd = || process::linux::dispatch_for_test(290, 0, 0, 0) as usize;
    let create_epoll = || process::linux::dispatch_for_test(291, 0, 0, 0) as usize;
    let (epfd, efd) = if eventfd_first {
        let efd = create_eventfd();
        (create_epoll(), efd)
    } else {
        let epfd = create_epoll();
        (epfd, create_eventfd())
    };
    check!(
        (3..task::fd_max()).contains(&epfd),
        "epoll_create1 returned {epfd:#x}"
    );
    check!(
        (3..task::fd_max()).contains(&efd),
        "eventfd2 returned {efd:#x}"
    );
    let mut event = [0u8; 12]; // packed `struct epoll_event`
    event[..4].copy_from_slice(&1u32.to_le_bytes());
    let ret = process::linux::dispatch_args_for_test(
        233,
        epfd as u64,
        EPOLL_CTL_ADD,
        efd as u64,
        event.as_ptr() as u64,
    );
    check!(ret == 0, "epoll_ctl(ADD) returned {ret:#x}");
    Ok((epfd, efd))
}

/// Ring the eventfd and require the epoll set to report it, then drain it.
fn rings_through(epfd: usize, efd: usize) -> Result<(), String> {
    task::fd_eventfd_write(efd, 1).map_err(io_err)?;
    let ready = task::fd_poll(epfd, POLLIN);
    let drained = task::fd_eventfd_read(efd).map_err(io_err)?;
    check!(drained == 1, "the eventfd held {drained}");
    check!(
        ready == Some(POLLIN),
        "the epoll set did not report its eventfd: {ready:?}"
    );
    Ok(())
}

/// Whether `efd` is still registered on `epfd` (`EPOLL_CTL_DEL` succeeds only then; it removes it).
fn registered(epfd: usize, efd: usize) -> bool {
    process::linux::dispatch_args_for_test(233, epfd as u64, EPOLL_CTL_DEL, efd as u64, 0) == 0
}

/// Run `body` as `child`, then come back to the kernel task.
fn as_child<T>(child: usize, body: impl FnOnce() -> T) -> T {
    task::harness::switch_current(child);
    let out = body();
    task::harness::switch_current(task::KERNEL_TASK);
    out
}

/// A child's `close` of its copies, and a child's `exec` (every close-on-exec descriptor), leave the
/// parent's interest; the parent's own close while a live child still holds both keeps it for the
/// child; the last holder's exit drops it.
pub fn epoll_child_close_keeps_parent_interest() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    let (epfd, efd) = doorbell_set(false)?;

    // A child closes the eventfd, then the epoll set: the parent's set still has its doorbell.
    let child = task::spawn_fork().map_err(to_string)?;
    let closed = as_child(child, || task::fd_close(efd) && task::fd_close(epfd));
    check!(closed, "the child could not close its copies");
    rings_through(epfd, efd).map_err(|e| format!("after a child's close: {e}"))?;
    task::harness::finish(child, 0);
    check!(
        task::reap_child().is_some(),
        "the first child is not reapable"
    );
    rings_through(epfd, efd).map_err(|e| format!("after a child's exit: {e}"))?;

    // A child's exec: both descriptors close-on-exec, closed in number order. An eventfd with the
    // lower number goes first, while the child still holds the instance: the case the old close got
    // wrong. (The other order is the explicit close above.)
    let (exec_epfd, exec_efd) = doorbell_set(true)?;
    check!(
        exec_efd < exec_epfd,
        "the eventfd ({exec_efd}) is not below the instance ({exec_epfd})"
    );
    check!(
        task::fd_set_cloexec(exec_efd, true) && task::fd_set_cloexec(exec_epfd, true),
        "FD_CLOEXEC refused"
    );
    let child = task::spawn_fork().map_err(to_string)?;
    let closed = as_child(child, task::fd_close_cloexec);
    check!(closed >= 2, "the child's exec closed {closed} descriptors");
    rings_through(exec_epfd, exec_efd).map_err(|e| format!("after a child's exec: {e}"))?;
    task::harness::finish(child, 0);
    check!(
        task::reap_child().is_some(),
        "the second child is not reapable"
    );
    check!(
        task::fd_close(exec_efd) && task::fd_close(exec_epfd),
        "closing the exec pair failed"
    );

    // The parent closes its eventfd while a child still holds both: the interest is the child's now,
    // and goes with the child's exit.
    let child = task::spawn_fork().map_err(to_string)?;
    let epoll = match task::fd_clone(epfd) {
        Some(task::Fd::Epoll { ref epoll }) => Arc::clone(epoll),
        _ => return Err(String::from("no epoll instance behind the descriptor")),
    };
    check!(
        task::fd_close(efd),
        "the parent could not close its eventfd"
    );
    check!(
        epoll.len() == 1,
        "the parent's close took the child's interest ({} left)",
        epoll.len()
    );
    task::harness::finish(child, 0);
    check!(
        task::reap_child().is_some(),
        "the third child is not reapable"
    );
    check!(
        epoll.is_empty(),
        "the last holder's exit left {} interests",
        epoll.len()
    );
    drop(epoll);

    // With no child, the parent's close drops the interest (a reused number starts clean).
    let efd = process::linux::dispatch_for_test(290, 0, 0, 0) as usize;
    let mut event = [0u8; 12];
    event[..4].copy_from_slice(&1u32.to_le_bytes());
    let ret = process::linux::dispatch_args_for_test(
        233,
        epfd as u64,
        EPOLL_CTL_ADD,
        efd as u64,
        event.as_ptr() as u64,
    );
    check!(ret == 0, "the second ADD returned {ret:#x}");
    check!(task::fd_close(efd), "closing the second eventfd failed");
    check!(
        !registered(epfd, efd),
        "the parent's close left its interest behind"
    );
    check!(task::fd_close(epfd), "closing the epoll set failed");
    check!(fds_clean(), "a descriptor was left open");
    Ok(())
}

/// The parent closes its descriptor while a forked child holds both it and the instance (the
/// interest is the child's then), and the child lets go of the instance first: nobody owns the
/// interest any more, and it must leave rather than keep its file open for good. The same when the
/// child exits holding the descriptor but not the instance.
pub fn epoll_unowned_interest_leaves() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    for exit_holding_file in [false, true] {
        let (epfd, efd) = doorbell_set(false)?;
        let epoll = match task::fd_clone(epfd) {
            Some(task::Fd::Epoll { ref epoll }) => Arc::clone(epoll),
            _ => return Err(String::from("no epoll instance behind the descriptor")),
        };
        let child = task::spawn_fork().map_err(to_string)?;
        check!(
            task::fd_close(efd),
            "the parent could not close its eventfd"
        );
        check!(
            epoll.len() == 1,
            "the parent's close took the child's interest"
        );
        // The instance first, as an exec closes it when its number is the lower one.
        let closed = as_child(child, || task::fd_close(epfd));
        check!(closed, "the child could not close the instance");
        check!(
            epoll.is_empty(),
            "an interest nobody owns survived the child's close of the instance"
        );
        if !exit_holding_file {
            check!(
                as_child(child, || task::fd_close(efd)),
                "the child could not close its eventfd"
            );
        }
        task::harness::finish(child, 0);
        check!(task::reap_child().is_some(), "the child is not reapable");
        check!(epoll.is_empty(), "the exit left {} interests", epoll.len());
        drop(epoll);
        check!(task::fd_close(epfd), "closing the instance failed");
    }
    check!(fds_clean(), "a descriptor was left open");
    Ok(())
}

/// Many children fork, close their copies (half by `close`, half by `exec`) and exit, as a browser
/// starting and restarting its engine's processes: the parent's doorbell rings through every time,
/// and no task slot or descriptor leaks.
pub fn epoll_soak_fork_close() -> Result<(), String> {
    fresh()?;
    task::harness::reset();
    let slots = task::free_slots();
    let (epfd, efd) = doorbell_set(false)?;
    check!(
        task::fd_set_cloexec(efd, true) && task::fd_set_cloexec(epfd, true),
        "FD_CLOEXEC refused"
    );
    for round in 0..300 {
        let child = task::spawn_fork().map_err(|e| format!("round {round}: {}", to_string(e)))?;
        if round % 2 == 0 {
            as_child(child, || {
                task::fd_close(efd);
                task::fd_close(epfd);
            });
        } else {
            as_child(child, task::fd_close_cloexec);
        }
        rings_through(epfd, efd).map_err(|e| format!("round {round}: {e}"))?;
        task::harness::finish(child, 0);
        check!(
            task::reap_child().is_some(),
            "round {round}: the child is not reapable"
        );
    }
    rings_through(epfd, efd)?;
    check!(
        registered(epfd, efd),
        "the interest did not survive the soak"
    );
    check!(
        task::fd_close(efd) && task::fd_close(epfd),
        "cleanup failed"
    );
    check!(fds_clean(), "the soak leaked a descriptor");
    check!(
        task::free_slots() == slots,
        "{} task slots free after the soak, {slots} before",
        task::free_slots()
    );
    Ok(())
}
