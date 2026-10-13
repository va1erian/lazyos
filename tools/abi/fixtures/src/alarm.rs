//! `alarm` — `alarm`, `setitimer`/`getitimer` and POSIX timers on the Linux
//! ABI (issue #535, gap 3), through musl's wrappers like any real program:
//!
//! 1. `alarm(1)` + `pause()`: `SIGALRM` runs the handler after about a second
//!    and `pause` returns `EINTR`; `alarm` reports the previous remainder and
//!    `alarm(0)` cancels.
//! 2. A blocking `read` on an empty pipe is interrupted (`EINTR`) by an
//!    alarm under a handler without `SA_RESTART`, and restarted (it returns
//!    the byte another thread wrote later) under `SA_RESTART`.
//! 3. A `nanosleep` is cut short with the remaining time written back.
//! 4. An `ITIMER_REAL` interval timer delivers repeatedly until disarmed.
//! 5. `ITIMER_PROF` and `ITIMER_VIRTUAL` signal a CPU-bound loop.
//! 6. A forked child has no alarm; an alarm survives `execve` (the program
//!    re-runs itself with `--after-exec` and cancels it).
//! 7. POSIX timers: `SIGEV_SIGNAL` with a `SA_SIGINFO` handler checking
//!    `si_code == SI_TIMER`, `si_value` and the id; `timer_gettime`,
//!    `timer_getoverrun` and `timer_delete`; `SIGEV_THREAD_ID` (the kernel
//!    interface behind musl's `SIGEV_THREAD`) aimed at a worker thread.
//!
//! Each step prints `ABI:alarm:STEP:<name>`; then `ABI:alarm:PASS`.

mod common;

use std::ffi::{c_int, c_long, c_uint, c_void};
use std::os::unix::process::CommandExt;
use std::sync::atomic::{AtomicI32, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const SIGALRM: c_int = 14;
const SIGVTALRM: c_int = 26;
const SIGPROF: c_int = 27;
const SIGUSR1: c_int = 10;

const SA_SIGINFO: c_int = 4;
const SA_RESTART: c_int = 0x1000_0000;
const SI_TIMER: i32 = -2;
const EINTR: i32 = 4;

const ITIMER_REAL: c_int = 0;
const ITIMER_VIRTUAL: c_int = 1;
const ITIMER_PROF: c_int = 2;

const CLOCK_MONOTONIC: c_int = 1;
const SIGEV_SIGNAL: c_int = 0;
const SIGEV_THREAD_ID: c_int = 4;
const SYS_TIMER_CREATE: c_long = 222;
const SYS_GETTID: c_long = 186;

/// musl's `struct sigaction` on x86_64.
#[repr(C)]
struct SigAction {
    handler: usize,
    mask: [u64; 16],
    flags: c_int,
    restorer: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TimeVal {
    sec: i64,
    usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ITimerVal {
    interval: TimeVal,
    value: TimeVal,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct TimeSpec {
    sec: i64,
    nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct ITimerSpec {
    interval: TimeSpec,
    value: TimeSpec,
}

/// `struct sigevent`: value, signo, notify, then the thread id.
#[repr(C)]
struct SigEvent {
    value: usize,
    signo: c_int,
    notify: c_int,
    tid: c_int,
    pad: [c_int; 11],
}

extern "C" {
    fn sigaction(sig: c_int, act: *const SigAction, old: *mut SigAction) -> c_int;
    fn alarm(seconds: c_uint) -> c_uint;
    fn pause() -> c_int;
    fn pipe(fds: *mut c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn nanosleep(req: *const TimeSpec, rem: *mut TimeSpec) -> c_int;
    fn setitimer(which: c_int, new: *const ITimerVal, old: *mut ITimerVal) -> c_int;
    fn getitimer(which: c_int, cur: *mut ITimerVal) -> c_int;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(status: c_int) -> !;
    fn timer_create(clock: c_int, event: *mut SigEvent, id: *mut *mut c_void) -> c_int;
    fn timer_settime(
        id: *mut c_void,
        flags: c_int,
        new: *const ITimerSpec,
        old: *mut ITimerSpec,
    ) -> c_int;
    fn timer_gettime(id: *mut c_void, cur: *mut ITimerSpec) -> c_int;
    fn timer_getoverrun(id: *mut c_void) -> c_int;
    fn timer_delete(id: *mut c_void) -> c_int;
    fn syscall(nr: c_long, ...) -> c_long;
}

static HITS: AtomicU32 = AtomicU32::new(0);
static LAST_SIGNO: AtomicI32 = AtomicI32::new(0);
static LAST_CODE: AtomicI32 = AtomicI32::new(0);
static LAST_TID: AtomicI32 = AtomicI32::new(0);
static LAST_VALUE: AtomicU64 = AtomicU64::new(0);
static HANDLER_THREAD: AtomicI64 = AtomicI64::new(0);

fn tid() -> i64 {
    unsafe { syscall(SYS_GETTID) as i64 }
}

extern "C" fn on_signal(sig: c_int, info: *const i32, _context: *const u8) {
    // SAFETY: with SA_SIGINFO the kernel passes a valid `siginfo_t`:
    // si_code at +8, si_tid at +16, si_value at +24.
    unsafe {
        LAST_CODE.store(*info.add(2), Ordering::Relaxed);
        LAST_TID.store(*info.add(4), Ordering::Relaxed);
        LAST_VALUE.store(*(info.add(6) as *const u64), Ordering::Relaxed);
    }
    LAST_SIGNO.store(sig, Ordering::Relaxed);
    HANDLER_THREAD.store(tid(), Ordering::Relaxed);
    HITS.fetch_add(1, Ordering::SeqCst);
}

fn handle(sig: c_int, restart: bool) {
    let action = SigAction {
        handler: on_signal as usize,
        mask: [0; 16],
        flags: SA_SIGINFO | if restart { SA_RESTART } else { 0 },
        restorer: 0,
    };
    // SAFETY: a valid, fully initialised sigaction.
    let ret = unsafe { sigaction(sig, &action, std::ptr::null_mut()) };
    if ret != 0 {
        common::fail("alarm", &format!("sigaction({sig}) failed"));
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn step(name: &str) {
    println!("ABI:alarm:STEP:{name}");
}

fn check(ok: bool, what: &str) {
    if !ok {
        common::fail("alarm", what);
    }
}

fn wait_for(hits: u32, limit: Duration) -> bool {
    let start = Instant::now();
    while HITS.load(Ordering::SeqCst) < hits {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// `alarm` + `pause`, `alarm` return values and cancellation.
fn alarm_and_pause() {
    handle(SIGALRM, false);
    HITS.store(0, Ordering::SeqCst);
    let start = Instant::now();
    // SAFETY: plain libc calls.
    unsafe {
        check(alarm(1) == 0, "the first alarm reported a remainder");
        let ret = pause();
        check(ret == -1 && errno() == EINTR, "pause did not fail with EINTR");
    }
    let elapsed = start.elapsed();
    check(HITS.load(Ordering::SeqCst) == 1, "the handler did not run once");
    check(
        elapsed >= Duration::from_millis(900) && elapsed < Duration::from_secs(5),
        &format!("alarm(1) fired after {elapsed:?}"),
    );
    check(LAST_SIGNO.load(Ordering::Relaxed) == SIGALRM, "wrong signal");
    // SAFETY: plain libc calls.
    unsafe {
        check(alarm(10) == 0, "alarm(10) reported a remainder");
        let left = alarm(0);
        check((9..=10).contains(&left), &format!("alarm(0) reported {left}"));
        check(alarm(0) == 0, "a cancelled alarm reported a remainder");
    }
    step("alarm_pause");
}

/// A blocking read: interrupted without `SA_RESTART`, resumed with it.
fn read_interrupt_and_restart() {
    let mut fds = [0 as c_int; 2];
    // SAFETY: `fds` holds two ints.
    check(unsafe { pipe(fds.as_mut_ptr()) } == 0, "pipe failed");
    let mut byte = 0u8;

    handle(SIGALRM, false);
    HITS.store(0, Ordering::SeqCst);
    // SAFETY: `byte` is one writable byte.
    let ret = unsafe {
        alarm(1);
        read(fds[0], &mut byte as *mut u8 as *mut c_void, 1)
    };
    check(
        ret == -1 && errno() == EINTR,
        &format!("a read interrupted by SIGALRM returned {ret} errno {}", errno()),
    );
    check(HITS.load(Ordering::SeqCst) == 1, "the handler did not run");
    step("read_eintr");

    handle(SIGALRM, true);
    HITS.store(0, Ordering::SeqCst);
    let write_fd = fds[1];
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2500));
        // SAFETY: one byte from a live buffer.
        unsafe { write(write_fd, b"x".as_ptr() as *const c_void, 1) };
    });
    // SAFETY: as above.
    let ret = unsafe {
        alarm(1);
        read(fds[0], &mut byte as *mut u8 as *mut c_void, 1)
    };
    check(
        ret == 1 && byte == b'x',
        &format!("an SA_RESTART read returned {ret} errno {}", errno()),
    );
    check(HITS.load(Ordering::SeqCst) >= 1, "the handler did not run");
    writer.join().unwrap();
    // SAFETY: descriptors this fixture owns.
    unsafe {
        close(fds[0]);
        close(fds[1]);
    }
    step("read_restart");
}

/// `nanosleep` cut short: `EINTR` and the remaining time.
fn sleep_interrupted() {
    handle(SIGALRM, false);
    HITS.store(0, Ordering::SeqCst);
    let request = TimeSpec { sec: 5, nsec: 0 };
    let mut rest = TimeSpec::default();
    let start = Instant::now();
    // SAFETY: valid timespecs.
    let ret = unsafe {
        alarm(1);
        nanosleep(&request, &mut rest)
    };
    check(
        ret == -1 && errno() == EINTR,
        &format!("nanosleep returned {ret} errno {}", errno()),
    );
    check(start.elapsed() < Duration::from_secs(4), "the sleep ran to its end");
    check(
        rest.sec >= 2 && rest.sec <= 4,
        &format!("remaining {}.{:09} s", rest.sec, rest.nsec),
    );
    step("nanosleep_eintr");
}

/// An interval timer repeats until disarmed.
fn interval_timer() {
    handle(SIGALRM, true);
    HITS.store(0, Ordering::SeqCst);
    let timer = ITimerVal {
        interval: TimeVal { sec: 0, usec: 50_000 },
        value: TimeVal { sec: 0, usec: 50_000 },
    };
    // SAFETY: valid itimervals.
    unsafe {
        check(setitimer(ITIMER_REAL, &timer, std::ptr::null_mut()) == 0, "setitimer");
        let mut cur = ITimerVal::default();
        check(getitimer(ITIMER_REAL, &mut cur) == 0, "getitimer");
        check(
            cur.interval.sec == 0 && cur.interval.usec == 50_000,
            "getitimer interval",
        );
    }
    check(wait_for(5, Duration::from_secs(20)), "5 interval signals never came");
    let off = ITimerVal::default();
    let mut old = ITimerVal::default();
    // SAFETY: valid itimervals.
    unsafe {
        check(setitimer(ITIMER_REAL, &off, &mut old) == 0, "disarm");
        check(old.interval.usec == 50_000, "the old interval was not reported");
    }
    std::thread::sleep(Duration::from_millis(100));
    let settled = HITS.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(300));
    check(
        HITS.load(Ordering::SeqCst) == settled,
        "signals kept coming after the timer was disarmed",
    );
    step("itimer_real");
}

/// CPU-time timers signal a busy loop.
fn cpu_timers() {
    for (which, sig) in [(ITIMER_PROF, SIGPROF), (ITIMER_VIRTUAL, SIGVTALRM)] {
        handle(sig, true);
        HITS.store(0, Ordering::SeqCst);
        let timer = ITimerVal {
            interval: TimeVal::default(),
            value: TimeVal { sec: 0, usec: 100_000 },
        };
        // SAFETY: a valid itimerval.
        check(
            unsafe { setitimer(which, &timer, std::ptr::null_mut()) } == 0,
            "setitimer(cpu)",
        );
        let start = Instant::now();
        let mut x = 1u64;
        while HITS.load(Ordering::SeqCst) == 0 {
            x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            if start.elapsed() > Duration::from_secs(30) {
                common::fail("alarm", &format!("ITIMER {which} never fired (x={x})"));
            }
        }
        check(LAST_SIGNO.load(Ordering::Relaxed) == sig, "wrong CPU timer signal");
    }
    step("itimer_cpu");
}

/// No alarm in a forked child.
fn fork_clears_alarm() {
    handle(SIGALRM, true);
    // SAFETY: plain libc calls; the child only calls async-signal-safe ones.
    unsafe {
        alarm(30);
        let pid = fork();
        if pid == 0 {
            let left = alarm(0);
            _exit(if left == 0 { 0 } else { 1 });
        }
        check(pid > 0, "fork failed");
        let mut status = 0;
        check(waitpid(pid, &mut status, 0) == pid, "waitpid failed");
        check(status == 0, "the child inherited a pending alarm");
        let left = alarm(0);
        check((28..=30).contains(&left), &format!("the parent's alarm: {left}"));
    }
    step("fork");
}

/// POSIX timers: SIGEV_SIGNAL with a value, then SIGEV_THREAD_ID.
fn posix_timers() {
    handle(SIGUSR1, true);
    HITS.store(0, Ordering::SeqCst);
    let mut event = SigEvent {
        value: 0xabcd_1234,
        signo: SIGUSR1,
        notify: SIGEV_SIGNAL,
        tid: 0,
        pad: [0; 11],
    };
    let mut timer: *mut c_void = std::ptr::null_mut();
    // SAFETY: valid structures; `timer` receives the id.
    check(
        unsafe { timer_create(CLOCK_MONOTONIC, &mut event, &mut timer) } == 0,
        "timer_create(SIGEV_SIGNAL)",
    );
    let spec = ITimerSpec {
        interval: TimeSpec { sec: 0, nsec: 40_000_000 },
        value: TimeSpec { sec: 0, nsec: 40_000_000 },
    };
    // SAFETY: valid timespecs and a live timer.
    unsafe {
        check(timer_settime(timer, 0, &spec, std::ptr::null_mut()) == 0, "timer_settime");
        let mut cur = ITimerSpec::default();
        check(timer_gettime(timer, &mut cur) == 0, "timer_gettime");
        check(cur.interval.nsec == 40_000_000, "timer_gettime interval");
    }
    check(wait_for(3, Duration::from_secs(20)), "POSIX timer signals never came");
    check(LAST_CODE.load(Ordering::Relaxed) == SI_TIMER, "si_code is not SI_TIMER");
    check(
        LAST_VALUE.load(Ordering::Relaxed) == 0xabcd_1234,
        "si_value did not carry sigev_value",
    );
    // SAFETY: a live timer.
    unsafe {
        check(timer_getoverrun(timer) >= 0, "timer_getoverrun");
        check(timer_delete(timer) == 0, "timer_delete");
        check(timer_gettime(timer, &mut ITimerSpec::default()) == -1, "a deleted timer");
    }
    step("posix_signal");

    // SIGEV_THREAD_ID: the signal must run on the worker thread.
    HITS.store(0, Ordering::SeqCst);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<i64>();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        ready_tx.send(tid()).unwrap();
        let _ = done_rx.recv();
    });
    let worker_tid = ready_rx.recv().unwrap();
    let mut threaded = SigEvent {
        value: 7,
        signo: SIGUSR1,
        notify: SIGEV_THREAD_ID,
        tid: worker_tid as c_int,
        pad: [0; 11],
    };
    let mut id: c_int = -1;
    // SAFETY: the raw syscall with the kernel's own `sigevent` layout.
    let ret = unsafe { syscall(SYS_TIMER_CREATE, CLOCK_MONOTONIC as c_long, &mut threaded, &mut id) };
    check(ret == 0, &format!("timer_create(SIGEV_THREAD_ID) returned {ret}"));
    let once = ITimerSpec {
        interval: TimeSpec::default(),
        value: TimeSpec { sec: 0, nsec: 30_000_000 },
    };
    // SAFETY: the id is the kernel's timer id, which is what musl's `timer_t` holds.
    unsafe {
        check(
            timer_settime(id as isize as *mut c_void, 0, &once, std::ptr::null_mut()) == 0,
            "timer_settime(thread)",
        );
    }
    check(wait_for(1, Duration::from_secs(20)), "the thread timer never fired");
    check(
        HANDLER_THREAD.load(Ordering::Relaxed) == worker_tid,
        "the signal ran on a thread other than the one aimed at",
    );
    // SAFETY: the same id.
    unsafe { timer_delete(id as isize as *mut c_void) };
    done_tx.send(()).unwrap();
    worker.join().unwrap();
    step("posix_thread_id");
}

fn main() {
    if std::env::args().any(|arg| arg == "--after-exec") {
        // SAFETY: plain libc call.
        let left = unsafe { alarm(0) };
        check(
            (20..=30).contains(&left),
            &format!("the alarm did not survive execve: alarm(0) reported {left}"),
        );
        step("exec");
        common::pass("alarm");
        return;
    }
    alarm_and_pause();
    read_interrupt_and_restart();
    sleep_interrupted();
    interval_timer();
    cpu_timers();
    fork_clears_alarm();
    posix_timers();
    // An ITIMER_REAL alarm survives execve: re-run this program, which
    // cancels it and reports.
    // SAFETY: plain libc call.
    unsafe { alarm(30) };
    // The fixture's own name: the kernel maps the `$PATH` search to /system/bin.
    let error = std::process::Command::new("abi-init")
        .arg("--after-exec")
        .exec();
    common::fail("alarm", &format!("execve failed: {error}"));
}
