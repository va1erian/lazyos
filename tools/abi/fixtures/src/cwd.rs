//! `cwd` — the per-task working directory (issue #365): `chdir`, `getcwd` and
//! relative names through the `std` calls that use them (`set_current_dir`,
//! `current_dir`, `fs::write`, `read_dir(".")`, `rename`, ...). It works in
//! `/tmp` and, when a data volume is mounted, in `/data` too, and reports each
//! finished directory as `ABI:cwd:ROUND:<base>` so the bench can insist on
//! `/data` when it attached a disk.
//!
//! A child process is started by name (`execve` of this same program) to prove
//! the directory survives `fork` + `execve`, and that the child's own `chdir`
//! does not move the parent.

mod common;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const NAME: &str = "cwd";
/// The fixture's own on-disk name, so a child `execve` finds it again.
const PROGRAM: &str = "INIT.ELF";
const CHILD: &str = "cwd-child";
/// `ENOTDIR`, which `set_current_dir` on a file must report.
const ENOTDIR: i32 = 20;
/// The exit code of a child that found itself in the wrong directory.
const WRONG_DIR: i32 = 7;

fn check(ok: bool, what: &str) -> Result<(), String> {
    if ok {
        Ok(())
    } else {
        Err(what.to_string())
    }
}

fn io<T>(result: std::io::Result<T>, what: &str) -> Result<T, String> {
    result.map_err(|error| format!("{what}: {error}"))
}

/// `mkdir`, tolerating a directory an earlier run left behind (ext2 cannot
/// remove directories yet, so a reused data disk still has them).
fn mkdir(path: &str) -> Result<(), String> {
    match fs::create_dir(path) {
        Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => {
            Err(format!("mkdir {path}: {error}"))
        }
        _ => Ok(()),
    }
}

/// The working directory is exactly `want`.
fn cwd_is(want: &str, when: &str) -> Result<(), String> {
    let got: PathBuf = io(env::current_dir(), "getcwd")?;
    check(got == Path::new(want), &format!("cwd {when}: {got:?}, not {want:?}"))
}

/// Whether a data volume is mounted (`/proc/mounts` lists `/data`).
fn has_data_mount() -> bool {
    fs::read_to_string("/proc/mounts")
        .map(|mounts| mounts.lines().any(|line| line.split_whitespace().nth(1) == Some("/data")))
        .unwrap_or(false)
}

/// The child: it must start in the directory its parent was in, and moving
/// away must not be seen by the parent.
fn child(want: &str) -> ! {
    let here = env::current_dir().ok();
    let moved = env::set_current_dir("/").is_ok();
    let code = if here.as_deref() == Some(Path::new(want)) && moved {
        0
    } else {
        WRONG_DIR
    };
    std::process::exit(code);
}

/// Everything the fixture checks, inside `<base>/cwd-abi`.
fn round(base: &str) -> Result<(), String> {
    let work = format!("{base}/cwd-abi");
    mkdir(&work)?;
    io(env::set_current_dir(&work), "chdir work")?;
    cwd_is(&work, "after chdir")?;

    // A relative create lands in the working directory.
    io(fs::write("f", b"relative"), "write f")?;
    let absolute = io(fs::read(format!("{work}/f")), "read the absolute name")?;
    check(absolute == b"relative", "f is not under the working directory")?;
    mkdir("sub")?;

    // Relative and folded moves.
    io(env::set_current_dir("sub"), "chdir sub")?;
    cwd_is(&format!("{work}/sub"), "in sub")?;
    io(env::set_current_dir(".."), "chdir ..")?;
    cwd_is(&work, "back from sub")?;
    io(env::set_current_dir("./sub/../sub/.."), "chdir folded")?;
    cwd_is(&work, "after a folded path")?;

    // `.` lists the working directory, and stats as a directory.
    let names: Vec<String> = io(fs::read_dir("."), "read_dir .")?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    check(
        names.iter().any(|n| n == "f") && names.iter().any(|n| n == "sub"),
        &format!("read_dir(.) listed {names:?}"),
    )?;
    check(io(fs::metadata("."), "stat .")?.is_dir(), "stat(.) is not a directory")?;

    io(fs::rename("f", "sub/g"), "rename f sub/g")?;
    check(io(fs::metadata("sub/g"), "stat sub/g")?.len() == 8, "sub/g has the wrong size")?;
    check(fs::metadata("f").is_err(), "f survived the rename")?;

    // Errors leave the directory alone.
    check(env::set_current_dir("nope").is_err(), "chdir to a missing directory")?;
    let not_dir = env::set_current_dir("sub/g").err().and_then(|e| e.raw_os_error());
    check(not_dir == Some(ENOTDIR), &format!("chdir to a file gave {not_dir:?}"))?;
    cwd_is(&work, "after failed chdirs")?;

    // `..` stops at the root.
    io(env::set_current_dir("/"), "chdir /")?;
    io(env::set_current_dir("../../.."), "chdir ../../..")?;
    cwd_is("/", "above the root")?;
    io(env::set_current_dir(&work), "chdir work again")?;

    // A child, started by name, inherits it across fork + execve.
    let status = io(Command::new(PROGRAM).args([CHILD, work.as_str()]).status(), "spawn child")?;
    check(status.code() == Some(0), &format!("the child ended {status:?}"))?;
    cwd_is(&work, "after the child moved")?;

    // Tidy up from outside. Only `/tmp` can lose directories: ext2 has no
    // `rmdir` yet.
    io(fs::remove_file("sub/g"), "unlink sub/g")?;
    if base == "/tmp" {
        io(fs::remove_dir("sub"), "rmdir sub")?;
    }
    io(env::set_current_dir(base), "chdir base")?;
    if base == "/tmp" {
        io(fs::remove_dir(&work), "rmdir work")?;
    }
    println!("ABI:{NAME}:ROUND:{base}");
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some(CHILD) {
        child(args.get(2).map(String::as_str).unwrap_or(""));
    }
    let mut bases = vec!["/tmp"];
    if has_data_mount() {
        bases.push("/data");
    }
    for base in bases {
        if let Err(reason) = round(base) {
            common::fail(NAME, &format!("{base}: {reason}"));
        }
    }
    common::pass(NAME);
}
