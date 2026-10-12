//! `bigapp` — what one browser-class program can hold (issue #774).
//!
//! A WebKit2 browser is a few processes and a few dozen threads, with a few
//! hundred MB resident and a pile of descriptors. This fixture measures the
//! ceilings LazyOS gives a single program, each pushed until the kernel says no:
//!
//!   (a) threads: `clone` until it fails (each thread blocks on a futex, as a
//!       browser's idle workers do), then all are woken and joined;
//!   (b) memory: anonymous `mmap`, every page touched, until `ENOMEM`; then
//!       all of it is unmapped;
//!   (c) descriptors: `dup` until `EMFILE`, plus what `getrlimit`/`setrlimit`
//!       (`RLIMIT_NOFILE`) say about it;
//!   (d) four forked children each do a smaller round of (a)-(c) at once, as
//!       the processes of a browser would.
//!
//! The program runs as the session user (uid 1000), the one the per-uid quotas
//! cap. The ceilings are the measurement, so they never fail the row; what does is
//! an unclean stop: a call that fails with an unexpected errno, memory that
//! cannot be touched or unmapped, descriptors that do not close, a child
//! that does not reach its (smaller) targets, or a limit that cannot be set
//! to a lower value. Prints
//! `ABI:bigapp:threads=<n>:mem=<MiB>:fds=<n>:PASS`; every phase also prints a
//! `BIGAPP:` progress line, so a crash shows where the program was.

mod common;

use core::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Builder, JoinHandle};

unsafe extern "C" {
    fn mmap(
        addr: *mut c_void,
        len: usize,
        prot: i32,
        flags: i32,
        fd: i32,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn dup(fd: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn fork() -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn setuid(uid: u32) -> i32;
    fn getrlimit(resource: i32, rlim: *mut [u64; 2]) -> i32;
    fn setrlimit(resource: i32, rlim: *const [u64; 2]) -> i32;
    fn __errno_location() -> *mut i32;
}

const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_PRIVATE: i32 = 0x02;
const MAP_ANONYMOUS: i32 = 0x20;
const RLIMIT_NOFILE: i32 = 7;

const ENOMEM: i32 = 12;
const EMFILE: i32 = 24;

const MIB: usize = 1024 * 1024;
const PAGE: usize = 4096;
/// One mapping: small enough to land near the ceiling, big enough that a
/// whole run is a few hundred calls.
const CHUNK: usize = 8 * MIB;
/// Thread stacks: a browser's workers reserve far more, but only touch a few
/// pages; this keeps the thread count (not memory) the thing being measured.
const THREAD_STACK: usize = 64 * 1024;

/// Safety valves, well above any ceiling: a kernel that never refuses is a
/// failure to report, not a loop to run forever.
const MAX_THREADS: usize = 4000;
const MAX_MEM_MIB: usize = 8 * 1024;
const MAX_FDS: usize = 100_000;

/// What each of the four children must reach (a "smaller version").
const CHILDREN: usize = 4;
const CHILD_THREADS: usize = 24;
const CHILD_MEM_MIB: usize = 64;
const CHILD_FDS: usize = 128;

fn errno() -> i32 {
    // SAFETY: musl's `__errno_location` returns the calling thread's errno.
    unsafe { *__errno_location() }
}

/// Where a ceiling ended: the count reached and the errno that stopped it
/// (`0` when a cap or a target did).
struct Reached {
    count: usize,
    errno: i32,
}

/// Idle workers parked on one futex word (`Condvar`), like a browser's pool.
struct Gate {
    open: Mutex<bool>,
    wake: Condvar,
}

/// Spawn threads until the kernel refuses or `cap` is reached, then release
/// and join them all. The errno of the refusal is `EAGAIN` when `clone` fails.
fn threads(cap: usize) -> Result<Reached, String> {
    let gate = Arc::new(Gate {
        open: Mutex::new(false),
        wake: Condvar::new(),
    });
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    let mut stopped = 0;
    while workers.len() < cap {
        let gate = Arc::clone(&gate);
        let spawned = Builder::new().stack_size(THREAD_STACK).spawn(move || {
            let mut open = gate.open.lock().unwrap();
            while !*open {
                open = gate.wake.wait(open).unwrap();
            }
        });
        match spawned {
            Ok(handle) => workers.push(handle),
            Err(error) => {
                stopped = error.raw_os_error().unwrap_or(-1);
                break;
            }
        }
    }
    let count = workers.len();
    *gate.open.lock().unwrap() = true;
    gate.wake.notify_all();
    for worker in workers {
        worker
            .join()
            .map_err(|_| "a worker thread panicked".to_string())?;
    }
    Ok(Reached {
        count,
        errno: stopped,
    })
}

/// Map and touch `CHUNK`-sized regions until `ENOMEM` (or `cap_mib`), then
/// unmap them all. Returns MiB reached.
fn memory(cap_mib: usize) -> Result<Reached, String> {
    let mut regions: Vec<*mut u8> = Vec::new();
    let mut stopped = 0;
    while regions.len() * CHUNK / MIB < cap_mib {
        // SAFETY: an anonymous private mapping; the result is checked.
        let base = unsafe {
            mmap(
                core::ptr::null_mut(),
                CHUNK,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base as isize == -1 {
            stopped = errno();
            break;
        }
        let base = base as *mut u8;
        for page in (0..CHUNK).step_by(PAGE) {
            // SAFETY: `page` is inside the mapping just made.
            unsafe { base.add(page).write_volatile(0xA5) };
        }
        regions.push(base);
    }
    if stopped != 0 && stopped != ENOMEM {
        return Err(format!("mmap failed with errno {stopped}, not ENOMEM"));
    }
    let count = regions.len() * CHUNK / MIB;
    for &base in &regions {
        // SAFETY: the page was touched above and the mapping is still ours.
        if unsafe { base.read_volatile() } != 0xA5 {
            return Err("a touched page lost its contents".to_string());
        }
        // SAFETY: `base` is a live `CHUNK` mapping from this function.
        if unsafe { munmap(base as *mut c_void, CHUNK) } != 0 {
            return Err(format!("munmap failed with errno {}", errno()));
        }
    }
    Ok(Reached {
        count,
        errno: stopped,
    })
}

/// `dup` descriptor 1 until `EMFILE` (or `cap`), then close them all. The
/// count is what the program could add to its three standard streams.
fn descriptors(cap: usize) -> Result<Reached, String> {
    let mut open: Vec<i32> = Vec::new();
    let mut stopped = 0;
    while open.len() < cap {
        // SAFETY: `dup` on an open descriptor; the result is checked.
        let fd = unsafe { dup(1) };
        if fd < 0 {
            stopped = errno();
            break;
        }
        open.push(fd);
    }
    if stopped != 0 && stopped != EMFILE {
        return Err(format!("dup failed with errno {stopped}, not EMFILE"));
    }
    let count = open.len();
    for fd in open {
        // SAFETY: closing a descriptor this function opened.
        if unsafe { close(fd) } != 0 {
            return Err(format!("close({fd}) failed with errno {}", errno()));
        }
    }
    Ok(Reached {
        count,
        errno: stopped,
    })
}

/// `RLIMIT_NOFILE` as the kernel reports it, and whether the soft limit can
/// be lowered and raised back to the hard one (what a browser does at start).
/// Returns `(soft, hard)`, or the reason it misbehaves.
fn nofile_limits() -> Result<(u64, u64), String> {
    let mut now = [0u64; 2];
    // SAFETY: `getrlimit` writes two u64s.
    if unsafe { getrlimit(RLIMIT_NOFILE, &mut now) } != 0 {
        return Err(format!("getrlimit failed with errno {}", errno()));
    }
    let (soft, hard) = (now[0], now[1]);
    // Raising the soft limit to the hard one must succeed (a no-op here).
    let raise = [hard, hard];
    // SAFETY: `setrlimit` reads two u64s.
    if unsafe { setrlimit(RLIMIT_NOFILE, &raise) } != 0 {
        return Err(format!(
            "setrlimit({hard},{hard}) failed with errno {}",
            errno()
        ));
    }
    // Lowering the soft limit is honoured, and the new limit is enforced.
    let low = (hard / 2).max(64);
    let lower = [low, hard];
    // SAFETY: as above.
    if unsafe { setrlimit(RLIMIT_NOFILE, &lower) } != 0 {
        return Err(format!(
            "setrlimit({low},{hard}) failed with errno {}",
            errno()
        ));
    }
    let held = descriptors(MAX_FDS)?;
    if held.count + 3 != low as usize {
        return Err(format!(
            "soft limit {low}: {} descriptors opened, wanted {}",
            held.count + 3,
            low
        ));
    }
    // Restoring the old soft limit works while it is at most the hard one.
    let restore = [soft, hard];
    // SAFETY: as above.
    if unsafe { setrlimit(RLIMIT_NOFILE, &restore) } != 0 {
        return Err(format!(
            "restoring the soft limit failed with errno {}",
            errno()
        ));
    }
    // The hard limit cannot be raised past the kernel's.
    let over = [hard + 1, hard + 1];
    // SAFETY: as above.
    if unsafe { setrlimit(RLIMIT_NOFILE, &over) } == 0 {
        return Err("a hard limit above the kernel's was accepted".to_string());
    }
    Ok((soft, hard))
}

/// A child: a smaller round that must reach its targets.
fn child_round(index: usize) -> Result<(), String> {
    let threads = threads(CHILD_THREADS)?;
    let memory = memory(CHILD_MEM_MIB)?;
    let fds = descriptors(CHILD_FDS)?;
    println!(
        "BIGAPP:child{index}:threads={}:mem={}:fds={}",
        threads.count, memory.count, fds.count
    );
    if threads.count < CHILD_THREADS {
        return Err(format!("only {} of {CHILD_THREADS} threads", threads.count));
    }
    if memory.count < CHILD_MEM_MIB {
        return Err(format!("only {} of {CHILD_MEM_MIB} MiB", memory.count));
    }
    if fds.count < CHILD_FDS {
        return Err(format!("only {} of {CHILD_FDS} descriptors", fds.count));
    }
    Ok(())
}

/// Fork [`CHILDREN`] children that all run their rounds at once; the failure
/// of the first one that did not finish cleanly, if any.
fn children() -> Result<(), String> {
    let mut pids = Vec::new();
    for index in 0..CHILDREN {
        // SAFETY: the parent has joined every thread, so `fork` is safe.
        let pid = unsafe { fork() };
        if pid < 0 {
            return Err(format!("fork failed with errno {}", errno()));
        }
        if pid == 0 {
            let code = match child_round(index) {
                Ok(()) => 0,
                Err(reason) => {
                    println!("BIGAPP:child{index}:FAIL:{reason}");
                    1
                }
            };
            std::process::exit(code);
        }
        pids.push(pid);
    }
    let mut failed = None;
    for (index, pid) in pids.into_iter().enumerate() {
        let mut status = 0;
        // SAFETY: waiting for a child this function forked.
        if unsafe { waitpid(pid, &mut status, 0) } != pid {
            return Err(format!("waitpid({pid}) failed with errno {}", errno()));
        }
        if status != 0 && failed.is_none() {
            failed = Some(format!("child {index} ended with status {status:#x}"));
        }
    }
    failed.map_or(Ok(()), Err)
}

/// The session user (`user`, uid 1000): the per-uid quotas do not cap root,
/// and a browser runs as the logged-in user.
const SESSION_UID: u32 = 1000;

fn run() -> Result<String, String> {
    // SAFETY: drops the program's uid; a failure is checked.
    if unsafe { setuid(SESSION_UID) } != 0 {
        return Err(format!(
            "setuid({SESSION_UID}) failed with errno {}",
            errno()
        ));
    }
    println!("BIGAPP:threads:start");
    let threads = threads(MAX_THREADS)?;
    println!("BIGAPP:threads={}:errno={}", threads.count, threads.errno);
    if threads.errno == 0 {
        return Err(format!("{MAX_THREADS} threads and no limit"));
    }

    println!("BIGAPP:mem:start");
    let memory = memory(MAX_MEM_MIB)?;
    println!("BIGAPP:mem={}:errno={}", memory.count, memory.errno);
    if memory.errno == 0 {
        return Err(format!("{MAX_MEM_MIB} MiB and no limit"));
    }

    println!("BIGAPP:fds:start");
    let fds = descriptors(MAX_FDS)?;
    println!("BIGAPP:fds={}:errno={}", fds.count + 3, fds.errno);
    if fds.errno == 0 {
        return Err(format!("{MAX_FDS} descriptors and no limit"));
    }

    println!("BIGAPP:children:start");
    children()?;

    // The descriptor limit as the program sees it, and what it may do to it.
    let (soft, hard) = nofile_limits()?;
    println!("BIGAPP:rlimit_nofile={soft}/{hard}");

    // Everything was given back: the program can do it all again.
    let again = threads_again(threads.count)?;
    if again {
        Ok(format!(
            "threads={}:mem={}:fds={}",
            threads.count,
            memory.count,
            fds.count + 3
        ))
    } else {
        Err("the thread ceiling shrank after the children exited".to_string())
    }
}

/// Whether `want` threads can be started again (nothing leaked a slot).
fn threads_again(want: usize) -> Result<bool, String> {
    Ok(threads(want)?.count >= want)
}

fn main() {
    match run() {
        Ok(numbers) => println!("ABI:bigapp:{numbers}:PASS"),
        Err(reason) => common::fail("bigapp", &reason),
    }
}
