//! `compat` — the calls real programs lean on, end to end through musl:
//! `SIGPIPE`, `SA_RESTART` (a `signal()` handler does not turn a blocked `read` into
//! `EINTR`, `siginterrupt` does), `waitpid` of one specific child among
//! several, `WIFSIGNALED`, threads sharing the descriptor table
//! (`CLONE_FILES`), `select`, `MSG_PEEK`, `flock`, `isatty` on a pipe,
//! `getpwuid`, `/proc/self/exe`, `getrlimit` and `sysinfo`.
//!
//! Each step prints `ABI:compat:STEP:<name>` when it passes; the first
//! failure ends the run with `ABI:compat:FAIL:<step>:<detail>`.

mod common;

use std::ffi::{c_char, c_int, c_long, c_void, CStr};
use std::fs::File;
use std::io::Write;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd};
use std::sync::atomic::{AtomicUsize, Ordering};

const NAME: &str = "compat";
const SIGUSR1: c_int = 10;
const SIGTERM: c_int = 15;
const SIGPIPE: c_int = 13;
const EPIPE: c_int = 32;
const EINTR: c_int = 4;
const EAGAIN: c_int = 11;
const LOCK_EX: c_int = 2;
const LOCK_NB: c_int = 4;
const MSG_PEEK: c_int = 2;

#[repr(C)]
struct Passwd {
    name: *const c_char,
    passwd: *const c_char,
    uid: u32,
    gid: u32,
    gecos: *const c_char,
    dir: *const c_char,
    shell: *const c_char,
}

extern "C" {
    fn signal(sig: c_int, handler: extern "C" fn(c_int)) -> usize;
    fn siginterrupt(sig: c_int, flag: c_int) -> c_int;
    #[link_name = "signal"]
    fn signal_raw(sig: c_int, handler: usize) -> usize;
    fn fork() -> c_int;
    fn getppid() -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
    fn raise(sig: c_int) -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn _exit(code: c_int) -> !;
    fn usleep(usec: u32) -> c_int;
    fn pipe(fds: *mut c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, len: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn isatty(fd: c_int) -> c_int;
    fn flock(fd: c_int, op: c_int) -> c_int;
    fn socketpair(domain: c_int, kind: c_int, protocol: c_int, sv: *mut c_int) -> c_int;
    fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn select(n: c_int, r: *mut u64, w: *mut u64, e: *mut u64, tv: *mut c_long) -> c_int;
    fn getpwuid(uid: u32) -> *const Passwd;
    fn readlink(path: *const c_char, buf: *mut c_char, len: usize) -> isize;
    fn getrlimit(resource: c_int, limit: *mut u64) -> c_int;
    fn sysinfo(info: *mut u8) -> c_int;
    fn __errno_location() -> *mut c_int;
}

static HANDLED: AtomicUsize = AtomicUsize::new(0);

extern "C" fn on_usr1(_: c_int) {
    HANDLED.fetch_add(1, Ordering::SeqCst);
}

fn errno() -> c_int {
    // SAFETY: musl's `__errno_location` returns this thread's errno slot.
    unsafe { *__errno_location() }
}

fn step(name: &str, result: Result<(), String>) {
    match result {
        Ok(()) => println!("ABI:{NAME}:STEP:{name}"),
        Err(detail) => common::fail(NAME, &format!("{name}:{detail}")),
    }
}

/// A child that signals this process after 100 ms, then writes one byte to
/// `fd` 100 ms later. Returns its pid.
fn signaller(fd: c_int) -> c_int {
    // SAFETY: the child only makes async-signal-safe calls before `_exit`.
    unsafe {
        let pid = fork();
        if pid == 0 {
            usleep(100_000);
            kill(getppid(), SIGUSR1);
            usleep(100_000);
            write(fd, b"x".as_ptr().cast(), 1);
            _exit(0);
        }
        pid
    }
}

/// `read` on a pipe while a handler runs: restarted under `signal()`
/// (`SA_RESTART`), `EINTR` after `siginterrupt(sig, 1)`.
fn restart() -> Result<(), String> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: plain libc calls on descriptors this test owns.
    unsafe {
        signal(SIGUSR1, on_usr1);
        if pipe(fds.as_mut_ptr()) != 0 {
            return Err("pipe".into());
        }
        let child = signaller(fds[1]);
        let mut byte = 0u8;
        let got = read(fds[0], (&mut byte as *mut u8).cast(), 1);
        waitpid(child, std::ptr::null_mut(), 0);
        if got != 1 || HANDLED.load(Ordering::SeqCst) != 1 {
            return Err(format!(
                "SA_RESTART read {got} errno {} handled {}",
                errno(),
                HANDLED.load(Ordering::SeqCst)
            ));
        }
        siginterrupt(SIGUSR1, 1);
        let child = signaller(fds[1]);
        let got = read(fds[0], (&mut byte as *mut u8).cast(), 1);
        let err = errno();
        waitpid(child, std::ptr::null_mut(), 0);
        close(fds[0]);
        close(fds[1]);
        if got != -1 || err != EINTR {
            return Err(format!("siginterrupt read {got} errno {err}"));
        }
    }
    Ok(())
}

/// `waitpid` names one child among several; a signal death is `WIFSIGNALED`.
fn wait_specific() -> Result<(), String> {
    // SAFETY: fork children that only sleep, exit or raise.
    unsafe {
        let slow = fork();
        if slow == 0 {
            usleep(150_000);
            _exit(3);
        }
        let fast = fork();
        if fast == 0 {
            _exit(4);
        }
        let mut status = 0;
        let got = waitpid(slow, &mut status, 0);
        if got != slow || status != 3 << 8 {
            return Err(format!("waitpid({slow}) gave {got} status {status:#x}"));
        }
        let got = waitpid(fast, &mut status, 0);
        if got != fast || status != 4 << 8 {
            return Err(format!("waitpid({fast}) gave {got} status {status:#x}"));
        }
        let killed = fork();
        if killed == 0 {
            raise(SIGTERM);
            _exit(0);
        }
        waitpid(killed, &mut status, 0);
        if status & 0x7f != SIGTERM {
            return Err(format!("SIGTERM child status {status:#x}"));
        }
    }
    Ok(())
}

/// A write to a pipe with no reader raises `SIGPIPE` (which kills by
/// default) and, with the signal ignored, fails with `EPIPE`.
fn sigpipe() -> Result<(), String> {
    // SAFETY: the child only writes, resets a disposition and exits.
    unsafe {
        let mut fds = [0 as c_int; 2];
        pipe(fds.as_mut_ptr());
        close(fds[0]);
        let child = fork();
        if child == 0 {
            signal_raw(SIGPIPE, 0); // SIG_DFL: std ignores it at startup
            write(fds[1], b"x".as_ptr().cast(), 1);
            _exit(0);
        }
        let mut status = 0;
        waitpid(child, &mut status, 0);
        if status & 0x7f != SIGPIPE {
            return Err(format!("default SIGPIPE child status {status:#x}"));
        }
        // Here SIGPIPE is ignored (std's startup), so the write fails.
        let got = write(fds[1], b"x".as_ptr().cast(), 1);
        let err = errno();
        close(fds[1]);
        if got != -1 || err != EPIPE {
            return Err(format!("ignored SIGPIPE write {got} errno {err}"));
        }
    }
    Ok(())
}

/// A descriptor a thread opens is usable by the main thread, and the main
/// thread's close reaches the thread (`CLONE_FILES`).
fn shared_table() -> Result<(), String> {
    let path = "/tmp/compat-shared";
    let fd = std::thread::spawn(move || File::create(path).map(IntoRawFd::into_raw_fd))
        .join()
        .map_err(|_| "thread panicked")?
        .map_err(|e| format!("create in thread: {e}"))?;
    // SAFETY: `fd` was opened by the thread into the shared table.
    let mut file = unsafe { File::from_raw_fd(fd) };
    file.write_all(b"shared")
        .map_err(|e| format!("write from main: {e}"))?;
    let raw = file.as_raw_fd();
    drop(file);
    let still_open = std::thread::spawn(move || {
        // SAFETY: probing a descriptor number; `isatty` only fails with EBADF.
        unsafe { isatty(raw) };
        errno()
    })
    .join()
    .map_err(|_| "thread panicked")?;
    if still_open != 9 {
        return Err(format!(
            "thread saw errno {still_open} after the close, want EBADF"
        ));
    }
    if std::fs::read(path).map_err(|e| e.to_string())? != b"shared" {
        return Err("contents".into());
    }
    Ok(())
}

/// `select`, `MSG_PEEK`, `flock`, `isatty` on a pipe.
fn io_calls() -> Result<(), String> {
    let mut fds = [0 as c_int; 2];
    // SAFETY: plain libc calls on descriptors this test owns.
    unsafe {
        socketpair(1, 1, 0, fds.as_mut_ptr());
        let mut set = 1u64 << fds[1];
        let mut tv = [0 as c_long, 20_000];
        if select(
            fds[1] + 1,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            tv.as_mut_ptr(),
        ) != 0
        {
            return Err("select on an empty socket".into());
        }
        send(fds[0], b"peek".as_ptr().cast(), 4, 0);
        let mut set = 1u64 << fds[1];
        if select(
            fds[1] + 1,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        ) != 1
        {
            return Err("select missed data".into());
        }
        let mut buf = [0u8; 8];
        let peeked = recv(fds[1], buf.as_mut_ptr().cast(), 8, MSG_PEEK);
        let read_back = recv(fds[1], buf.as_mut_ptr().cast(), 8, 0);
        if peeked != 4 || read_back != 4 {
            return Err(format!("peek {peeked} then read {read_back}"));
        }
        if isatty(fds[0]) != 0 {
            return Err("a socket is a tty".into());
        }
        close(fds[0]);
        close(fds[1]);
    }
    let path = "/tmp/compat-lock";
    let one = File::create(path).map_err(|e| e.to_string())?;
    let two = File::open(path).map_err(|e| e.to_string())?;
    // SAFETY: `flock` on two descriptors this test owns.
    unsafe {
        if flock(one.as_raw_fd(), LOCK_EX | LOCK_NB) != 0 {
            return Err(format!("first flock errno {}", errno()));
        }
        if flock(two.as_raw_fd(), LOCK_EX | LOCK_NB) == 0 || errno() != EAGAIN {
            return Err("a second exclusive flock was granted".into());
        }
    }
    Ok(())
}

/// The account database, `/proc/self/exe`, rlimits and `sysinfo`.
fn system_info() -> Result<(), String> {
    // SAFETY: musl returns a pointer to a static `struct passwd` or null.
    unsafe {
        let entry = getpwuid(0);
        if entry.is_null() {
            return Err("getpwuid(0) found no account".into());
        }
        let dir = CStr::from_ptr((*entry).dir).to_string_lossy();
        if !dir.starts_with('/') || (*entry).uid != 0 {
            return Err(format!("root account dir {dir}"));
        }
        let mut buf = [0 as c_char; 256];
        let n = readlink(c"/proc/self/exe".as_ptr(), buf.as_mut_ptr(), 255);
        if n <= 0 {
            return Err("readlink /proc/self/exe".into());
        }
        let mut limit = [0u64; 2];
        if getrlimit(7, limit.as_mut_ptr()) != 0 || limit[0] == 0 {
            return Err("getrlimit(NOFILE)".into());
        }
        let mut info = [0u8; 112];
        if sysinfo(info.as_mut_ptr()) != 0 || info[32..40].iter().all(|&b| b == 0) {
            return Err("sysinfo".into());
        }
    }
    Ok(())
}

fn main() {
    step("restart", restart());
    step("wait", wait_specific());
    step("sigpipe", sigpipe());
    step("shared_table", shared_table());
    step("io", io_calls());
    step("system", system_info());
    common::pass(NAME);
}
