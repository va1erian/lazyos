//! `forkwrite` — a process whose threads keep writing while another of its threads forks and execs.
//!
//! WebKit's UI process (webd) launches its engine processes this way: a thread that is not the
//! process's first calls `fork` and the child `execve`s at once, while the other threads go on
//! writing to the heap. After such launches webd aborted on a corrupt heap (the main run loop's
//! timer tree; it was WTF's, patch 0037 of `webkit/`). This fixture checks that no store of the
//! parent is lost or lands elsewhere across `fork`, the child's copy-on-write faults and its
//! `execve`: [`WRITERS`] threads each own an array of [`WORDS`] words and, generation after
//! generation, check that every word still holds what they stored last and store the next value,
//! and every store also counts in an atomic total and a mutex-guarded total, while a launcher
//! thread forks [`LAUNCHES`] children. Each child writes to a few pages (the copy-on-write side)
//! and `execve`s this program in `child` mode, which exits 0 at once. Meanwhile three threads sleep in
//! the kernel on one page (`forkwrite_blocked`) and get their data only after each fork.

mod common;
mod forkwrite_blocked;

use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

extern "C" {
    fn fork() -> i32;
    fn execv(path: *const i8, argv: *const *const i8) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(code: i32) -> !;
    fn __errno_location() -> *mut i32;
}

const NAME: &str = "forkwrite";
/// The fixture's path in the image (`/system/bin/abi-init`, as `procstress` finds it).
const PROGRAM: &str = "/system/bin/abi-init";
const WRITERS: usize = 6;
/// 4 MiB per writer: a thousand pages each, so every fork write-protects thousands of entries.
const WORDS: usize = 512 * 1024;
const LAUNCHES: usize = 300;
/// The run stops launching after this long, whatever the count: the bench captures every row at a
/// fixed time (`tools/abi/run.py --at 8` in CI, under TCG).
const BUDGET: Duration = Duration::from_secs(4);

fn errno() -> i32 {
    // SAFETY: musl's errno location is valid for the calling thread.
    unsafe { *__errno_location() }
}

/// The value word `index` holds after generation `generation` of writer `writer`.
fn value(writer: usize, generation: u64, index: usize) -> u64 {
    (writer as u64) << 56 | generation << 24 | index as u64
}

struct Shared {
    stop: AtomicBool,
    atomic_total: AtomicU64,
    locked_total: Mutex<u64>,
}

/// One writer: checks the previous generation, stores the next; the first word that is wrong, if any.
fn writer(id: usize, shared: Arc<Shared>) -> Result<(u64, u64), String> {
    let mut words = vec![0u64; WORDS];
    for (index, word) in words.iter_mut().enumerate() {
        *word = value(id, 0, index);
    }
    let mut generation = 0u64;
    let mut stores = 0u64;
    while !shared.stop.load(Ordering::Relaxed) {
        for index in 0..WORDS {
            let want = value(id, generation, index);
            if words[index] != want {
                return Err(format!(
                    "writer {id} word {index} (page {}) is {:#x}, not {want:#x}, generation {generation}",
                    index * 8 / 4096,
                    words[index]
                ));
            }
            words[index] = value(id, generation + 1, index);
            if index % 512 == 0 {
                shared.atomic_total.fetch_add(1, Ordering::Relaxed);
                *shared.locked_total.lock().unwrap() += 1;
                stores += 1;
            }
        }
        generation += 1;
    }
    Ok((generation, stores))
}

/// Forks and execs one child; its exit status, or why it could not be had.
fn launch(
    path: &CString,
    argv: &[*const i8; 3],
    blocked: &forkwrite_blocked::Blocked,
    round: u64,
) -> Result<i32, String> {
    // SAFETY: the child only touches its stack and calls execv/_exit (async-signal-safe), as
    // WebKit's launcher does; the parent waits for the pid it got.
    let pid = unsafe { fork() };
    if pid < 0 {
        return Err(format!("fork failed with errno {}", errno()));
    }
    if pid == 0 {
        // The copy-on-write side: the child writes before it execs.
        let mut scratch = [0u8; 3 * 4096];
        for (at, byte) in scratch.iter_mut().enumerate().step_by(4096) {
            // SAFETY: a volatile store to the child's own stack array, so it is not optimised out.
            unsafe { std::ptr::write_volatile(byte, at as u8) };
        }
        // SAFETY: `path` and `argv` were made before the fork and are NUL-terminated.
        unsafe {
            execv(path.as_ptr(), argv.as_ptr());
            _exit(127);
        }
    }
    blocked.feed(round);
    let mut status = 0;
    // SAFETY: waiting for the child this call forked.
    if unsafe { waitpid(pid, &mut status, 0) } != pid {
        return Err(format!("waitpid({pid}) failed with errno {}", errno()));
    }
    Ok(status)
}

fn launcher(shared: Arc<Shared>) -> Result<usize, String> {
    let path = CString::new(PROGRAM).unwrap();
    let mode = CString::new("child").unwrap();
    let argv = [path.as_ptr(), mode.as_ptr(), std::ptr::null()];
    let blocked =
        forkwrite_blocked::start().ok_or("no pipe or socket pair for the blocked calls")?;
    std::thread::sleep(Duration::from_millis(20));
    let started = Instant::now();
    let mut done = 0;
    let result = loop {
        if done >= LAUNCHES || started.elapsed() >= BUDGET {
            break Ok(done);
        }
        let status = match launch(&path, &argv, &blocked, done as u64 + 1) {
            Ok(status) => status,
            Err(reason) => break Err(reason),
        };
        if status != 0 {
            break Err(format!("child {done} ended with status {status:#x}"));
        }
        if let Err(reason) = blocked.settle(done as u64 + 1) {
            break Err(reason);
        }
        done += 1;
    };
    blocked.stop();
    shared.stop.store(true, Ordering::Relaxed);
    result
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("child") {
        std::process::exit(0);
    }
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        atomic_total: AtomicU64::new(0),
        locked_total: Mutex::new(0),
    });
    let writers: Vec<_> = (0..WRITERS)
        .map(|id| {
            let shared = shared.clone();
            std::thread::spawn(move || writer(id, shared))
        })
        .collect();
    let launches = {
        let shared = shared.clone();
        std::thread::spawn(move || launcher(shared))
    }
    .join()
    .unwrap();
    let mut generations = 0;
    let mut stores = 0;
    let mut first: Option<String> = None;
    for handle in writers {
        match handle.join().unwrap() {
            Ok((g, s)) => {
                generations += g;
                stores += s;
            }
            Err(reason) => first = first.or(Some(reason)),
        }
    }
    let launches = match launches {
        Ok(n) => n,
        Err(reason) => common::fail(NAME, &reason),
    };
    if let Some(reason) = first {
        common::fail(NAME, &reason);
    }
    let atomic = shared.atomic_total.load(Ordering::Relaxed);
    let locked = *shared.locked_total.lock().unwrap();
    if atomic != stores || locked != stores {
        common::fail(
            NAME,
            &format!("counted {stores} stores, atomic total {atomic}, locked total {locked}"),
        );
    }
    println!("ABI:{NAME}:launches={launches}:generations={generations}:stores={stores}");
    common::report(NAME, launches > 0 && generations > 0, "nothing ran");
}
