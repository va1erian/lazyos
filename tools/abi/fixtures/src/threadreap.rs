//! `threadreap` — a child belongs to its parent's process (issue #859).
//!
//! WebKit's launcher forks an engine process on one thread and reaps it on a
//! dedicated reaper thread; on Linux any thread of the process may `waitpid`
//! for a child of the process. Here: a child forked by the main thread is
//! reaped by another thread (status intact), a child forked by a thread that
//! has since exited is still reapable by the main thread, a stranger process
//! gets `ECHILD`, and `prctl(PR_SET_PDEATHSIG)` ends a grandchild when its
//! parent exits.

mod common;

use std::thread;
use std::time::Duration;

#[repr(C)]
struct PollFd {
    fd: i32,
    events: i16,
    revents: i16,
}

unsafe extern "C" {
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(code: i32) -> !;
    fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32;
    fn pipe(fds: *mut i32) -> i32;
    fn close(fd: i32) -> i32;
    fn poll(fds: *mut PollFd, count: u64, timeout: i32) -> i32;
    fn read(fd: i32, buf: *mut u8, len: usize) -> isize;
    fn usleep(us: u32) -> i32;
}

const ECHILD: i32 = 10;
const PR_SET_PDEATHSIG: i32 = 1;
const SIGKILL: u64 = 9;
const POLLIN: i16 = 1;
const POLLHUP: i16 = 0x10;

fn errno() -> Option<i32> {
    std::io::Error::last_os_error().raw_os_error()
}

/// Fork a child that exits with `code` after a short nap.
fn fork_exit(code: i32) -> i32 {
    let pid = unsafe { fork() };
    if pid == 0 {
        unsafe {
            usleep(30_000);
            _exit(code)
        }
    }
    pid
}

fn wait_status(pid: i32) -> Result<i32, String> {
    let mut status = 0;
    let got = unsafe { waitpid(pid, &mut status, 0) };
    if got == pid {
        Ok(status)
    } else {
        Err(format!(
            "waitpid({pid}) returned {got}, errno {:?}",
            errno()
        ))
    }
}

fn run() -> Result<(), String> {
    // 1. Forked here, reaped by another thread.
    let child = fork_exit(7);
    if child <= 0 {
        return Err("fork failed".into());
    }
    let reaper = thread::spawn(move || wait_status(child));
    let status = reaper.join().map_err(|_| "reaper panicked")??;
    if status != 7 << 8 {
        return Err(format!("reaper saw status {status:#x}, expected 0x700"));
    }
    let mut dummy = 0;
    if unsafe { waitpid(child, &mut dummy, 1) } != -1 || errno() != Some(ECHILD) {
        return Err("the child was reaped twice".into());
    }

    // 2. Forked by a thread that exits before the child does.
    let forker = thread::spawn(|| fork_exit(9));
    let child = forker.join().map_err(|_| "forker panicked")?;
    if child <= 0 {
        return Err("fork in a thread failed".into());
    }
    let status = wait_status(child).map_err(|e| format!("after the forking thread exited: {e}"))?;
    if status != 9 << 8 {
        return Err(format!("status {status:#x}, expected 0x900"));
    }

    // 3. A stranger (a forked child) has no children of the parent's.
    let stranger = unsafe { fork() };
    if stranger == 0 {
        let mut s = 0;
        let ok = unsafe { waitpid(child, &mut s, 1) } == -1 && errno() == Some(ECHILD);
        unsafe { _exit(if ok { 0 } else { 1 }) }
    }
    if wait_status(stranger)? != 0 {
        return Err("a stranger could wait for another process's child".into());
    }

    // 4. PR_SET_PDEATHSIG: the grandchild dies with its parent. The pipe's
    // write end lives in the grandchild only; EOF here means it is gone.
    let mut fds = [0i32; 2];
    if unsafe { pipe(fds.as_mut_ptr()) } != 0 {
        return Err("pipe failed".into());
    }
    let middle = unsafe { fork() };
    if middle == 0 {
        let grandchild = unsafe { fork() };
        if grandchild == 0 {
            unsafe {
                close(fds[0]);
                prctl(PR_SET_PDEATHSIG, SIGKILL, 0, 0, 0);
                usleep(60_000_000); // killed long before this ends
                _exit(2)
            }
        }
        unsafe {
            usleep(200_000);
            _exit(0)
        }
    }
    unsafe { close(fds[1]) };
    wait_status(middle)?;
    let mut entry = PollFd {
        fd: fds[0],
        events: POLLIN,
        revents: 0,
    };
    let ready = unsafe { poll(&mut entry, 1, 15_000) };
    if ready != 1 || entry.revents & (POLLIN | POLLHUP) == 0 {
        return Err("the grandchild outlived its parent (PDEATHSIG)".into());
    }
    let mut byte = 0u8;
    if unsafe { read(fds[0], &mut byte, 1) } != 0 {
        return Err("unexpected data from the grandchild".into());
    }
    thread::sleep(Duration::from_millis(1));
    Ok(())
}

fn main() {
    match run() {
        Ok(()) => common::pass("threadreap"),
        Err(reason) => common::fail("threadreap", &reason),
    }
}
