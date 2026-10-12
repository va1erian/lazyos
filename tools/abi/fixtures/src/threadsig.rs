//! `threadsig` — per-thread signal masks and thread-directed signals, the
//! pattern a JavaScriptCore garbage collector uses to stop the world
//! (issue #772).
//!
//! A "collector" (the main thread) signals each of 8 mutator threads with
//! `pthread_kill` (`tgkill`); every handler must run on the thread it was
//! aimed at, park on a futex until the collector releases it, and — because
//! each handler blocks its signal on its own thread only — all eight sit
//! parked at once. With one process-wide mask the first parked handler would
//! hold the signal back from the other seven and the round would hang.
//! 1000 rounds, then a process-directed `SIGUSR2` with the main thread
//! blocking it, which must be handled by another thread.
//!
//! Prints `ABI:threadsig:PASS`.

mod common;

use std::os::unix::thread::JoinHandleExt;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};
use std::time::{Duration, Instant};

const SYS_RT_SIGACTION: u64 = 13;
const SYS_RT_SIGPROCMASK: u64 = 14;
const SYS_GETTID: u64 = 186;
const SYS_FUTEX: u64 = 202;
const SYS_KILL: u64 = 62;
const SYS_GETPID: u64 = 39;

const SIGUSR1: usize = 10;
const SIGUSR2: usize = 12;
const SIG_BLOCK: u64 = 0;
const SIG_UNBLOCK: u64 = 1;

const SA_SIGINFO: u64 = 0x4;
const SA_RESTORER: u64 = 0x0400_0000;
const SA_RESTART: u64 = 0x1000_0000;
/// `si_code` of a `tkill`/`tgkill`.
const SI_TKILL: i32 = -6;

const FUTEX_WAIT: u64 = 0;
const FUTEX_WAKE: u64 = 1;
const FUTEX_PRIVATE: u64 = 128;

const THREADS: usize = 8;
const ROUNDS: u32 = 1000;
/// How long the collector waits for one step of a round before giving up.
const STEP_TIMEOUT: Duration = Duration::from_secs(60);

/// The tid of each mutator, published before the first round.
static TIDS: [AtomicI32; THREADS] = [const { AtomicI32::new(0) }; THREADS];
/// Handlers that ran on each mutator.
static HANDLED: [AtomicU32; THREADS] = [const { AtomicU32::new(0) }; THREADS];
/// Handlers that ran on a thread other than the one aimed at, with a bad
/// `si_code`, or with the signal not blocked inside the handler.
static MISPLACED: AtomicU32 = AtomicU32::new(0);
static BAD_INFO: AtomicU32 = AtomicU32::new(0);
static NOT_BLOCKED: AtomicU32 = AtomicU32::new(0);

/// The round the collector is running, and the last round it released.
static ROUND: AtomicU32 = AtomicU32::new(0);
static RELEASED: AtomicU32 = AtomicU32::new(0);
/// Handlers parked, and handlers that resumed, in the current round.
static PARKED: AtomicU32 = AtomicU32::new(0);
static RESUMED: AtomicU32 = AtomicU32::new(0);
static STOP: AtomicBool = AtomicBool::new(false);

/// Who ran the process-directed `SIGUSR2` handler.
static USR2_TID: AtomicI32 = AtomicI32::new(0);
static USR2_RUNS: AtomicUsize = AtomicUsize::new(0);

core::arch::global_asm!(
    r#"
    .globl __abi_restore_rt
    __abi_restore_rt:
        mov rax, 15
        syscall
"#
);

unsafe extern "C" {
    fn __abi_restore_rt();
    fn pthread_kill(thread: u64, sig: i32) -> i32;
}

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64,
}

unsafe fn syscall6(nr: u64, args: [u64; 6]) -> i64 {
    let ret: i64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") nr as i64 => ret,
        in("rdi") args[0],
        in("rsi") args[1],
        in("rdx") args[2],
        in("r10") args[3],
        in("r8") args[4],
        in("r9") args[5],
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

fn gettid() -> i32 {
    unsafe { syscall6(SYS_GETTID, [0; 6]) as i32 }
}

fn install(sig: usize, handler: extern "C" fn(i32, *const i32, *const u8)) -> i64 {
    let action = KernelSigaction {
        handler: handler as usize,
        flags: SA_SIGINFO | SA_RESTART | SA_RESTORER,
        restorer: __abi_restore_rt as *const () as usize,
        mask: 0,
    };
    let args = [sig as u64, &action as *const _ as u64, 0, 8, 0, 0];
    unsafe { syscall6(SYS_RT_SIGACTION, args) }
}

/// `rt_sigprocmask(how, set)` for the calling thread; the previous mask.
fn sigprocmask(how: u64, set: u64) -> u64 {
    let mut old = 0u64;
    let args = [
        how,
        &set as *const u64 as u64,
        &mut old as *mut u64 as u64,
        8,
        0,
        0,
    ];
    unsafe { syscall6(SYS_RT_SIGPROCMASK, args) };
    old
}

fn futex_wait(word: &AtomicU32, expected: u32, timeout: Duration) {
    let ts = [timeout.as_secs(), u64::from(timeout.subsec_nanos())];
    let args = [
        word as *const AtomicU32 as u64,
        FUTEX_WAIT | FUTEX_PRIVATE,
        u64::from(expected),
        ts.as_ptr() as u64,
        0,
        0,
    ];
    unsafe { syscall6(SYS_FUTEX, args) };
}

fn futex_wake_all(word: &AtomicU32) {
    let args = [
        word as *const AtomicU32 as u64,
        FUTEX_WAKE | FUTEX_PRIVATE,
        i32::MAX as u64,
        0,
        0,
        0,
    ];
    unsafe { syscall6(SYS_FUTEX, args) };
}

/// Block until `word` reaches `target`, or fail the run after [`STEP_TIMEOUT`].
fn wait_for(word: &AtomicU32, target: u32, what: &str) {
    let start = Instant::now();
    loop {
        let seen = word.load(Ordering::SeqCst);
        if seen >= target {
            return;
        }
        if start.elapsed() > STEP_TIMEOUT {
            common::fail(
                "threadsig",
                &format!(
                    "round {}: {what}: {seen}/{target} after {:?}",
                    ROUND.load(Ordering::SeqCst),
                    STEP_TIMEOUT
                ),
            );
        }
        futex_wait(word, seen, Duration::from_millis(50));
    }
}

/// The stop-the-world handler: run on the targeted thread, park until the
/// collector releases the round.
extern "C" fn suspend_handler(sig: i32, info: *const i32, _ucontext: *const u8) {
    let me = gettid();
    match TIDS.iter().position(|tid| tid.load(Ordering::SeqCst) == me) {
        Some(index) => {
            HANDLED[index].fetch_add(1, Ordering::SeqCst);
        }
        None => {
            MISPLACED.fetch_add(1, Ordering::SeqCst);
        }
    }
    // `si_signo` at 0, `si_code` at 8: a `pthread_kill` is a `tgkill`.
    if sig != SIGUSR1 as i32
        || unsafe { *info } != SIGUSR1 as i32
        || unsafe { *info.add(2) } != SI_TKILL
    {
        BAD_INFO.fetch_add(1, Ordering::SeqCst);
    }
    // Blocked inside the handler, on this thread.
    if sigprocmask(SIG_BLOCK, 0) & (1 << (SIGUSR1 - 1)) == 0 {
        NOT_BLOCKED.fetch_add(1, Ordering::SeqCst);
    }
    let round = ROUND.load(Ordering::SeqCst);
    PARKED.fetch_add(1, Ordering::SeqCst);
    futex_wake_all(&PARKED);
    while RELEASED.load(Ordering::SeqCst) < round {
        let seen = RELEASED.load(Ordering::SeqCst);
        futex_wait(&RELEASED, seen, Duration::from_millis(50));
    }
    RESUMED.fetch_add(1, Ordering::SeqCst);
    futex_wake_all(&RESUMED);
}

/// Records which thread ran a process-directed signal.
extern "C" fn usr2_handler(_sig: i32, _info: *const i32, _ucontext: *const u8) {
    USR2_TID.store(gettid(), Ordering::SeqCst);
    USR2_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// A mutator: spins (taking its signals in user mode) or sleeps (taking them
/// on the way out of a syscall) until told to stop.
fn mutator(index: usize) {
    TIDS[index].store(gettid(), Ordering::SeqCst);
    while !STOP.load(Ordering::SeqCst) {
        if index % 2 == 0 {
            std::hint::spin_loop();
            std::thread::yield_now();
        } else {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

const SYS_CLOCK_NANOSLEEP: u64 = 230;
const SYS_SIGALTSTACK: u64 = 131;
const CLOCK_MONOTONIC: u64 = 1;
const SIGALRM: usize = 14;
const SA_ONSTACK: u64 = 0x0800_0000;
const EINTR: i64 = 4;

/// The sleeper's `clock_nanosleep` result and how long it took (ms).
static SLEEPER_TID: AtomicI32 = AtomicI32::new(0);
static SLEEPER_RET: AtomicI64 = AtomicI64::new(i64::MIN);
static SLEEPER_MS: AtomicU64 = AtomicU64::new(0);
static SPINNER_TID: AtomicI32 = AtomicI32::new(0);
static SPINNER_STOP: AtomicBool = AtomicBool::new(false);

/// Stack for `SA_ONSTACK` handlers, and where each of them ran.
const ALT_LEN: usize = 65536;
static ALT_BASE: AtomicUsize = AtomicUsize::new(0);
static ALT_RUNS: AtomicUsize = AtomicUsize::new(0);
static ALT_INSIDE: AtomicUsize = AtomicUsize::new(0);

extern "C" fn alarm_handler(_sig: i32, _info: *const i32, _ucontext: *const u8) {
    let marker = 0u8;
    let here = core::hint::black_box(&marker) as *const u8 as usize;
    let base = ALT_BASE.load(Ordering::SeqCst);
    if (base..base + ALT_LEN).contains(&here) {
        ALT_INSIDE.fetch_add(1, Ordering::SeqCst);
    }
    ALT_RUNS.fetch_add(1, Ordering::SeqCst);
}

/// Two consecutive `SA_ONSTACK` signals both run on the alternate stack.
fn altstack_phase(first: &mut Option<String>) {
    let memory: &'static mut [u8] = Box::leak(vec![0u8; ALT_LEN].into_boxed_slice());
    ALT_BASE.store(memory.as_ptr() as usize, Ordering::SeqCst);
    let stack = [memory.as_ptr() as u64, 0, ALT_LEN as u64];
    let rc = unsafe { syscall6(SYS_SIGALTSTACK, [stack.as_ptr() as u64, 0, 0, 0, 0, 0]) };
    check(first, rc == 0, || "sigaltstack failed".to_string());
    let action = KernelSigaction {
        handler: alarm_handler as usize,
        flags: SA_SIGINFO | SA_RESTART | SA_RESTORER | SA_ONSTACK,
        restorer: __abi_restore_rt as *const () as usize,
        mask: 0,
    };
    let args = [SIGALRM as u64, &action as *const _ as u64, 0, 8, 0, 0];
    check(
        first,
        unsafe { syscall6(SYS_RT_SIGACTION, args) } == 0,
        || "rt_sigaction SIGALRM failed".to_string(),
    );
    let pid = unsafe { syscall6(SYS_GETPID, [0; 6]) } as u64;
    for _ in 0..2 {
        unsafe { syscall6(SYS_KILL, [pid, SIGALRM as u64, 0, 0, 0, 0]) };
    }
    let (runs, inside) = (
        ALT_RUNS.load(Ordering::SeqCst),
        ALT_INSIDE.load(Ordering::SeqCst),
    );
    check(first, runs == 2 && inside == 2, || {
        format!(
            "{inside} of {runs} SA_ONSTACK handlers ran on the alternate stack, expected 2 of 2"
        )
    });
}

/// A thread asleep in `clock_nanosleep(2 s)` while a process-directed signal
/// goes to the process: if another thread takes it, the sleeper must not see
/// an interruption and sleeps the full time (+-200 ms). If the sleeper itself
/// ran the handler, `EINTR` is the right answer.
fn sleeper_phase(first: &mut Option<String>, main_tid: i32) {
    let sleeper = std::thread::spawn(|| {
        SLEEPER_TID.store(gettid(), Ordering::SeqCst);
        let ts = [2u64, 0];
        let start = Instant::now();
        let args = [CLOCK_MONOTONIC, 0, ts.as_ptr() as u64, 0, 0, 0];
        let ret = unsafe { syscall6(SYS_CLOCK_NANOSLEEP, args) };
        SLEEPER_MS.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
        SLEEPER_RET.store(ret, Ordering::SeqCst);
    });
    // Spawned second, so the lower slot is the sleeper's: it is the thread
    // the kernel prefers to wake. The spinner is the one in user mode.
    let spinner = std::thread::spawn(|| {
        SPINNER_TID.store(gettid(), Ordering::SeqCst);
        while !SPINNER_STOP.load(Ordering::SeqCst) {
            std::hint::spin_loop();
        }
    });
    let start = Instant::now();
    while SLEEPER_TID.load(Ordering::SeqCst) == 0 || SPINNER_TID.load(Ordering::SeqCst) == 0 {
        if start.elapsed() > STEP_TIMEOUT {
            common::fail("threadsig", "the sleeper phase did not start");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    std::thread::sleep(Duration::from_millis(200));
    let usr2 = 1u64 << (SIGUSR2 - 1);
    sigprocmask(SIG_BLOCK, usr2);
    let before = USR2_RUNS.load(Ordering::SeqCst);
    let pid = unsafe { syscall6(SYS_GETPID, [0; 6]) } as u64;
    unsafe { syscall6(SYS_KILL, [pid, SIGUSR2 as u64, 0, 0, 0, 0]) };
    let start = Instant::now();
    while USR2_RUNS.load(Ordering::SeqCst) == before && start.elapsed() < STEP_TIMEOUT {
        std::thread::sleep(Duration::from_millis(1));
    }
    let ran_on = USR2_TID.load(Ordering::SeqCst);
    SPINNER_STOP.store(true, Ordering::SeqCst);
    check(
        first,
        sleeper.join().is_ok() && spinner.join().is_ok(),
        || "a phase thread panicked".to_string(),
    );
    sigprocmask(SIG_UNBLOCK, usr2);
    let (ret, ms) = (
        SLEEPER_RET.load(Ordering::SeqCst),
        SLEEPER_MS.load(Ordering::SeqCst),
    );
    let sleeper_tid = SLEEPER_TID.load(Ordering::SeqCst);
    check(first, ran_on != main_tid && ran_on != 0, || {
        format!("the signal ran on tid {ran_on} (main {main_tid})")
    });
    println!(
        "ABI:threadsig:STEP:sleeper:{}:ret={ret}:ms={ms}",
        if ran_on == sleeper_tid {
            "self"
        } else {
            "sibling"
        }
    );
    if ran_on == sleeper_tid {
        check(first, ret == -EINTR, || {
            format!("the sleeper ran the handler, yet clock_nanosleep returned {ret}")
        });
    } else {
        check(first, ret == 0 && (1800..=2200).contains(&ms), || {
            format!("a sibling took the signal, yet the sleeper returned {ret} after {ms} ms (expected 0 after 2000 +-200)")
        });
    }
}

fn check(first: &mut Option<String>, ok: bool, reason: impl FnOnce() -> String) {
    if !ok && first.is_none() {
        *first = Some(reason());
    }
}

fn main() {
    let main_tid = gettid();
    if install(SIGUSR1, suspend_handler) != 0 || install(SIGUSR2, usr2_handler) != 0 {
        common::fail("threadsig", "rt_sigaction failed");
    }
    let workers: Vec<_> = (0..THREADS)
        .map(|index| std::thread::spawn(move || mutator(index)))
        .collect();
    let start = Instant::now();
    while TIDS.iter().any(|tid| tid.load(Ordering::SeqCst) == 0) {
        if start.elapsed() > STEP_TIMEOUT {
            common::fail("threadsig", "the mutators did not start");
        }
        std::thread::yield_now();
    }

    for round in 1..=ROUNDS {
        ROUND.store(round, Ordering::SeqCst);
        PARKED.store(0, Ordering::SeqCst);
        RESUMED.store(0, Ordering::SeqCst);
        for worker in &workers {
            let rc = unsafe { pthread_kill(worker.as_pthread_t(), SIGUSR1 as i32) };
            if rc != 0 {
                common::fail(
                    "threadsig",
                    &format!("round {round}: pthread_kill returned {rc}"),
                );
            }
        }
        // Every mutator parks in its handler at once.
        wait_for(&PARKED, THREADS as u32, "mutators parked");
        RELEASED.store(round, Ordering::SeqCst);
        futex_wake_all(&RELEASED);
        wait_for(&RESUMED, THREADS as u32, "mutators resumed");
    }

    // A process-directed signal the main thread blocks lands on another thread.
    let mut first: Option<String> = None;
    let usr2 = 1u64 << (SIGUSR2 - 1);
    sigprocmask(SIG_BLOCK, usr2);
    let pid = unsafe { syscall6(SYS_GETPID, [0; 6]) } as u64;
    check(
        &mut first,
        unsafe { syscall6(SYS_KILL, [pid, SIGUSR2 as u64, 0, 0, 0, 0]) } == 0,
        || "kill(SIGUSR2) failed".to_string(),
    );
    let start = Instant::now();
    while USR2_RUNS.load(Ordering::SeqCst) == 0 && start.elapsed() < STEP_TIMEOUT {
        std::thread::sleep(Duration::from_millis(1));
    }
    check(&mut first, USR2_RUNS.load(Ordering::SeqCst) == 1, || {
        format!(
            "SIGUSR2 handler ran {} times",
            USR2_RUNS.load(Ordering::SeqCst)
        )
    });
    let ran_on = USR2_TID.load(Ordering::SeqCst);
    check(
        &mut first,
        ran_on != main_tid && TIDS.iter().any(|t| t.load(Ordering::SeqCst) == ran_on),
        || {
            format!("SIGUSR2 ran on tid {ran_on}, not on a thread that did not block it (main {main_tid})")
        },
    );
    sigprocmask(SIG_UNBLOCK, usr2);

    STOP.store(true, Ordering::SeqCst);
    for worker in workers {
        check(&mut first, worker.join().is_ok(), || {
            "a mutator panicked".to_string()
        });
    }

    for (index, handled) in HANDLED.iter().enumerate() {
        let handled = handled.load(Ordering::SeqCst);
        check(&mut first, handled == ROUNDS, || {
            format!("mutator {index} handled {handled} of {ROUNDS} signals")
        });
    }
    check(&mut first, MISPLACED.load(Ordering::SeqCst) == 0, || {
        format!(
            "{} handlers ran on the wrong thread",
            MISPLACED.load(Ordering::SeqCst)
        )
    });
    check(&mut first, BAD_INFO.load(Ordering::SeqCst) == 0, || {
        format!(
            "{} handlers saw a wrong siginfo",
            BAD_INFO.load(Ordering::SeqCst)
        )
    });
    check(&mut first, NOT_BLOCKED.load(Ordering::SeqCst) == 0, || {
        format!(
            "{} handlers ran with the signal unblocked",
            NOT_BLOCKED.load(Ordering::SeqCst)
        )
    });
    sleeper_phase(&mut first, main_tid);
    altstack_phase(&mut first);

    // The mutators' masks never leaked into the collector's.
    check(&mut first, sigprocmask(SIG_BLOCK, 0) == 0, || {
        "the main thread's mask changed".to_string()
    });

    match first {
        Some(reason) => common::fail("threadsig", &reason),
        None => common::pass("threadsig"),
    }
}
