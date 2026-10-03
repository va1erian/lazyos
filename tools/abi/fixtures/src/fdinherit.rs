//! `fdinherit` — a file descriptor a parent opened for a redirection reaches
//! the child it `fork`s and `execve`s, and the child's `write`/`writev` land in
//! the file: the shell's `prog >/tmp/out 2>&1` and `prog >>/tmp/out`, done by
//! hand with raw syscalls, plus the same through `std::process::Command`.

mod common;

use std::ffi::CString;
use std::fs::OpenOptions;
use std::os::raw::{c_char, c_int, c_void};
use std::process::Command;

/// The fixture's own name (`/system/bin/abi-init`), so a child `execve`
/// finds it again through the `/system/bin` lookup.
const PROGRAM: &str = "abi-init";
const OUT: &str = "/tmp/fdinherit.out";

const O_WRONLY: c_int = 0o1;
const O_CREAT: c_int = 0o100;
const O_TRUNC: c_int = 0o1000;
const O_APPEND: c_int = 0o2000;

#[repr(C)]
struct IoVec {
    base: *const c_void,
    len: usize,
}

unsafe extern "C" {
    fn fork() -> c_int;
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn dup2(old: c_int, new: c_int) -> c_int;
    fn execve(path: *const c_char, argv: *const *const c_char, envp: *const *const c_char)
        -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn write(fd: c_int, buf: *const c_void, len: usize) -> isize;
    fn writev(fd: c_int, iov: *const IoVec, count: c_int) -> isize;
    fn _exit(code: c_int) -> !;
}

/// What one child run writes: `write` on stdout, `writev` on stdout, then
/// `write` on stderr (redirected to the same file by `2>&1`).
fn expected(tag: &str) -> String {
    format!("{tag}:write\n{tag}:writev-a{tag}:writev-b\n{tag}:stderr\n")
}

/// Child mode: write the run's lines through descriptors 1 and 2 and exit 0
/// only if every call wrote all its bytes.
fn child(tag: &str) -> ! {
    let line = format!("{tag}:write\n");
    let (a, b) = (format!("{tag}:writev-a"), format!("{tag}:writev-b\n"));
    let err = format!("{tag}:stderr\n");
    let iov = [
        IoVec {
            base: a.as_ptr().cast(),
            len: a.len(),
        },
        IoVec {
            base: b.as_ptr().cast(),
            len: b.len(),
        },
    ];
    // SAFETY: every pointer/length pair names a live Rust string or the iovec
    // array above, all of which outlive the calls.
    let ok = unsafe {
        write(1, line.as_ptr().cast(), line.len()) == line.len() as isize
            && writev(1, iov.as_ptr(), 2) == (a.len() + b.len()) as isize
            && write(2, err.as_ptr().cast(), err.len()) == err.len() as isize
    };
    // SAFETY: `_exit` takes no pointers.
    unsafe { _exit(if ok { 0 } else { 3 }) }
}

/// `fork`; in the child open `OUT` with `flags`, `dup2` it onto 1 and 2 and
/// (with `exec`) `execve` this program in child mode, else write in place.
/// Returns the child's exit status, or why it could not be had.
fn redirect_run(tag: &str, flags: c_int, exec: bool) -> Result<c_int, String> {
    let path = CString::new(OUT).unwrap();
    let program = CString::new(PROGRAM).unwrap();
    let (arg1, arg2) = (CString::new("child").unwrap(), CString::new(tag).unwrap());
    let argv = [
        program.as_ptr(),
        arg1.as_ptr(),
        arg2.as_ptr(),
        std::ptr::null(),
    ];
    let envp = [std::ptr::null()];
    // SAFETY: the child only calls async-signal-safe libc functions (open,
    // dup2, close, execve, write, _exit) on buffers prepared before the fork.
    let pid = unsafe { fork() };
    if pid < 0 {
        return Err("fork failed".into());
    }
    if pid == 0 {
        // SAFETY: as above; `path`, `argv` and `envp` are NUL-terminated and live.
        unsafe {
            let fd = open(path.as_ptr(), O_WRONLY | O_CREAT | flags, 0o644);
            if fd < 0 || dup2(fd, 1) != 1 || dup2(1, 2) != 2 {
                _exit(4);
            }
            close(fd);
            if !exec {
                child(tag);
            }
            execve(program.as_ptr(), argv.as_ptr(), envp.as_ptr());
            _exit(5);
        }
    }
    let mut status: c_int = 0;
    // SAFETY: `status` is a live c_int.
    if unsafe { waitpid(pid, &mut status, 0) } != pid {
        return Err("waitpid failed".into());
    }
    Ok(status)
}

/// Record a failed check; every failure is reported, not only the first.
fn check(failures: &mut Vec<String>, what: &str, ok: bool) {
    if !ok {
        failures.push(what.to_string());
    }
}

fn read_out() -> String {
    std::fs::read_to_string(OUT).unwrap_or_else(|e| format!("<read failed: {e}>"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("child") {
        child(args.get(2).map(String::as_str).unwrap_or("?"));
    }
    let mut failures = Vec::new();

    // `prog >OUT 2>&1` through fork + execve.
    let status = redirect_run("exec", O_TRUNC, true);
    check(
        &mut failures,
        &format!("exec run status {status:?}"),
        status == Ok(0),
    );
    let got = read_out();
    check(
        &mut failures,
        &format!("after exec >: {got:?}"),
        got == expected("exec"),
    );

    // `prog >>OUT 2>&1`: appended after the first run's bytes.
    let status = redirect_run("append", O_APPEND, true);
    check(
        &mut failures,
        &format!("append run status {status:?}"),
        status == Ok(0),
    );
    let got = read_out();
    let want = expected("exec") + &expected("append");
    check(
        &mut failures,
        &format!("after exec >>: {got:?}"),
        got == want,
    );

    // The same descriptor without the exec: a forked child writes through it.
    let status = redirect_run("fork", O_TRUNC, false);
    check(
        &mut failures,
        &format!("fork run status {status:?}"),
        status == Ok(0),
    );
    let got = read_out();
    check(
        &mut failures,
        &format!("after fork >: {got:?}"),
        got == expected("fork"),
    );

    // std's spawn with a `File` as stdout and stderr.
    let opened = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(OUT);
    match opened.and_then(|out| Ok((out.try_clone()?, out))) {
        Ok((err, out)) => {
            let status = Command::new(PROGRAM)
                .args(["child", "std"])
                .stdout(out)
                .stderr(err)
                .status();
            let ok = matches!(&status, Ok(s) if s.success());
            check(&mut failures, &format!("std run status {status:?}"), ok);
            let got = read_out();
            check(
                &mut failures,
                &format!("after std: {got:?}"),
                got == expected("std"),
            );
        }
        Err(e) => check(&mut failures, &format!("open for std: {e}"), false),
    }

    common::report("fdinherit", failures.is_empty(), &failures.join("; "));
}
