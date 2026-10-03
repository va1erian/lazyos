//! `CLONE_FILES` and `CLONE_FS`: a thread created with them sees every
//! descriptor and directory change its siblings make; one created without
//! them starts with a copy and diverges.

use super::*;

/// A thread of the current task with the given sharing.
fn thread(files: bool, fs: bool) -> Result<usize, String> {
    task::spawn_thread_sharing(
        "share",
        process::USER_STACK_TOP,
        0,
        0,
        task::ThreadShare { files, fs },
    )
    .map_err(|error| format!("spawn: {error}"))
}

/// The process the tests run in: a fork with a table of its own.
fn process() -> Result<usize, String> {
    let leader = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
    task::harness::switch_current(leader);
    Ok(leader)
}

fn kind(slot: usize, fd: u64) -> task::FdKind {
    task::harness::fd_kind_at(slot, fd as usize)
}

/// Opens, `dup2`, `FD_CLOEXEC` and closes in either thread show in the other,
/// and a pipe's last close (from either side) is still its last close.
pub fn clone_files_shares_table() -> Result<(), String> {
    fresh()?;
    let leader = process()?;
    let (r, w) = pipe()?;
    let peer = thread(true, false)?;
    check!(
        kind(peer, r) == task::FdKind::Pipe,
        "the thread did not inherit the pipe"
    );
    // Opened in the thread: visible to the leader.
    task::harness::switch_current(peer);
    let (r2, w2) = pipe()?;
    check!(sys(33, &[w2, 9]) == 9, "dup2 in the thread failed");
    check!(task::fd_set_cloexec(9, true), "FD_CLOEXEC");
    task::harness::switch_current(leader);
    check!(
        kind(leader, r2) == task::FdKind::Pipe && kind(leader, 9) == task::FdKind::Pipe,
        "the leader cannot see the thread's descriptors"
    );
    check!(
        task::harness::fd_cloexec_at(leader, 9),
        "FD_CLOEXEC did not travel"
    );
    // Closed in the leader: gone from the thread too, and the write end's
    // last close makes the reader see end-of-file.
    for fd in [w2, 9] {
        check!(sys(3, &[fd]) == 0, "close {fd}");
    }
    check!(
        kind(peer, 9) == task::FdKind::Closed,
        "close did not reach the thread"
    );
    let mut byte = [0u8; 1];
    check!(
        sys(0, &[r2, byte.as_mut_ptr() as u64, 1]) == 0,
        "no EOF after the shared close"
    );
    let groups = task::linuxstate::share_groups(peer);
    check!(
        groups.0 != 0 && groups.0 == task::linuxstate::share_groups(leader).0,
        "no shared group: {groups:?}"
    );
    let _ = (r, w);
    task::harness::reset();
    Ok(())
}

/// Without `CLONE_FILES` the thread gets a copy (the old code gave it a bare
/// terminal table): the descriptors exist, and later changes stay private.
pub fn clone_without_files_copies() -> Result<(), String> {
    fresh()?;
    let leader = process()?;
    let (r, _w) = pipe()?;
    let peer = thread(false, false)?;
    check!(
        kind(peer, r) == task::FdKind::Pipe,
        "the copy lacks the pipe"
    );
    check!(sys(3, &[r]) == 0, "close");
    check!(
        kind(peer, r) == task::FdKind::Pipe,
        "a private close reached the thread"
    );
    check!(
        kind(leader, r) == task::FdKind::Closed,
        "the close did not happen"
    );
    check!(
        task::linuxstate::share_groups(peer).0 == 0,
        "a private thread is in a group"
    );
    task::harness::reset();
    Ok(())
}

/// `CLONE_FS`: `chdir` in one thread moves the others; without it, not.
pub fn clone_fs_shares_cwd() -> Result<(), String> {
    fresh()?;
    let leader = process()?;
    let shared = thread(false, true)?;
    let private = thread(false, false)?;
    let tmp = cpath(fhs::mount::TMP);
    check!(sys(80, &[tmp.as_ptr() as u64]) == 0, "chdir /tmp");
    for (slot, want) in [
        (leader, fhs::mount::TMP),
        (shared, fhs::mount::TMP),
        (private, "/"),
    ] {
        task::harness::switch_current(slot);
        let cwd = task::cwd();
        check!(cwd == want, "slot {slot} is in {cwd}, want {want}");
    }
    task::harness::reset();
    Ok(())
}

/// Many open/dup/close rounds across three sharing threads: the tables stay
/// identical, no pipe end leaks, and dropping the threads frees everything.
pub fn fdshare_soak() -> Result<(), String> {
    fresh()?;
    let leader = process()?;
    let peers = [thread(true, true)?, thread(true, true)?];
    let live = crate::ipc::pipe::Pipe::live();
    let members = [leader, peers[0], peers[1]];
    for round in 0..600usize {
        task::harness::switch_current(members[round % 3]);
        let (r, w) = pipe()?;
        let extra = sys(32, &[w]);
        check!((extra as i64) > 0, "round {round}: dup {extra:#x}");
        task::harness::switch_current(members[(round + 1) % 3]);
        for fd in [r, w, extra] {
            check!(
                sys(3, &[fd]) == 0,
                "round {round}: close {fd} from a sibling"
            );
        }
        for &slot in &members {
            for fd in 3..task::harness::fd_table_len() as u64 {
                let (a, b) = (kind(slot, fd), kind(leader, fd));
                check!(a == b, "round {round}: slot {slot} fd {fd} diverged");
            }
        }
    }
    check!(
        crate::ipc::pipe::Pipe::live() == live,
        "pipes leaked: {} -> {}",
        live,
        crate::ipc::pipe::Pipe::live()
    );
    task::harness::reset();
    check!(
        crate::ipc::pipe::Pipe::live() <= live,
        "pipes outlived the threads"
    );
    Ok(())
}
