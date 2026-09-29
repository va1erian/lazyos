//! What a relative path means once a task has a working directory (issue
//! #365): every path-taking syscall resolves against it, on `/tmp` and on
//! `/data`, descriptor-relative names ignore it, and `fork`, `execve` and the
//! task's exit treat it as POSIX says. The soak repeats the whole cycle.

use super::attrs::{attrs, chmod};
use super::cwd::{chdir, cwd_is, getcwd, mkdir, open_dir, rmdir, ENOTDIR};
use super::inspect::{dents, parse};
use super::*;

const SYS_ACCESS: u64 = 21;
const SYS_EXECVE: u64 = 59;
const SYS_MKDIRAT: u64 = 258;
const SYS_NEWFSTATAT: u64 = 262;
const SYS_UNLINKAT: u64 = 263;
const SYS_RENAMEAT: u64 = 264;
const SYS_GETDENTS64: u64 = 217;

const AT_REMOVEDIR: u64 = 0x200;
const AT_EMPTY_PATH: u64 = 0x1000;
const O_DIRECTORY: u64 = 0o200000;

/// `newfstatat(dirfd, path, flags)`: the raw `struct stat`, or the raw error.
fn newfstatat(dirfd: u64, path: &str, flags: u64) -> Result<[u8; 144], u64> {
    let mut stat = [0u8; 144];
    let ret = process::linux::dispatch_args5_for_test(
        SYS_NEWFSTATAT,
        dirfd,
        cstr(path).as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        flags,
        0,
    );
    if ret == 0 {
        Ok(stat)
    } else {
        Err(ret)
    }
}

/// `(st_ino, st_size)` of what `newfstatat` names.
fn ino_size_at(dirfd: u64, path: &str, flags: u64) -> Result<(u64, u64), String> {
    let stat = newfstatat(dirfd, path, flags).map_err(|code| format!("{code:#x}"))?;
    Ok(ino_size(&stat))
}

/// `st_ino` and `st_size` of a raw `struct stat`.
fn ino_size(stat: &[u8; 144]) -> (u64, u64) {
    let word = |at: usize| u64::from_le_bytes(stat[at..at + 8].try_into().unwrap());
    (word(8), word(48))
}

/// The names in the directory `path`, read through `getdents64`.
fn names_in(path: &str) -> Result<Vec<String>, String> {
    let fd = open_dir(path);
    check!(fd < 16, "open({path:?}) returned {fd:#x}");
    let stream = dents(SYS_GETDENTS64, fd, 4096).map_err(|code| format!("{code:#x}"))?;
    close(fd);
    Ok(parse(&stream, false)?
        .into_iter()
        .map(|entry| entry.name)
        .collect())
}

/// One mount's worth of relative I/O: work in `<mount>/w` using relative names
/// only, and check after each step that the absolute name agrees.
fn relative_round(mount: &str) -> Result<(), String> {
    let work = format!("{mount}/w");
    check!(mkdir(&work) == 0 && chdir(&work) == 0, "set up {work}");

    put("f", b"hello")?;
    for name in [
        format!("{work}/f"),
        String::from("./f"),
        String::from("../w/f"),
    ] {
        check!(slurp(&name)? == b"hello", "{name} is not the file created");
    }
    check!(
        open("f", O_CREAT | O_EXCL | O_WRONLY) == errno(EEXIST),
        "O_EXCL"
    );
    check!(
        open("f", O_DIRECTORY) == errno(ENOTDIR),
        "O_DIRECTORY on a file"
    );

    check!(mkdir("sub") == 0, "mkdir sub");
    let listed = names_in(".")?;
    check!(
        ["f", "sub"]
            .iter()
            .all(|name| listed.iter().any(|got| got == name)),
        "listing . gave {listed:?}"
    );
    check!(
        names_in(&work)? == listed,
        "listing . differs from listing {work}"
    );

    check!(chmod("f", 0o600) == 0, "chmod f");
    check!(
        attrs(&format!("{work}/f"))?.mode == 0o600,
        "chmod went astray"
    );
    check!(attrs("f")?.mode == 0o600, "stat f");
    check!(path_call(SYS_TRUNCATE, "f", 2) == 0, "truncate f");
    check!(slurp("f")? == b"he", "truncated contents");
    check!(path_call(SYS_ACCESS, "f", 4) == 0, "access f");
    check!(
        path_call(SYS_ACCESS, "nope", 0) == errno(ENOENT),
        "access nope"
    );

    // The directory itself, three ways.
    let by_dot = newfstatat(AT_FDCWD, "", AT_EMPTY_PATH).map_err(|c| format!("{c:#x}"))?;
    let by_name = newfstatat(AT_FDCWD, &work, 0).map_err(|c| format!("{c:#x}"))?;
    check!(
        ino_size(&by_dot).0 == ino_size(&by_name).0,
        "the cwd's inode"
    );
    check!(
        newfstatat(AT_FDCWD, "", 0) == Err(errno(ENOENT)),
        "an empty path without AT_EMPTY_PATH"
    );
    check!(ino_size_at(AT_FDCWD, "f", 0)?.1 == 2, "newfstatat f");

    let (from, to) = (cstr("f"), cstr("sub/g"));
    let renamed = syscall(
        SYS_RENAMEAT,
        AT_FDCWD,
        from.as_ptr() as u64,
        AT_FDCWD,
        to.as_ptr() as u64,
    );
    check!(renamed == 0, "renameat f -> sub/g");
    check!(
        open("f", O_RDONLY) == errno(ENOENT),
        "f survived the rename"
    );
    check!(slurp("sub/g")? == b"he", "renamed contents");
    check!(
        slurp(&format!("{work}/sub/g"))? == b"he",
        "renamed, absolute"
    );

    let m = cstr("m");
    check!(
        syscall(SYS_MKDIRAT, AT_FDCWD, m.as_ptr() as u64, 0o755, 0) == 0,
        "mkdirat"
    );
    let at = |name: &str, flags: u64| {
        syscall(SYS_UNLINKAT, AT_FDCWD, cstr(name).as_ptr() as u64, flags, 0)
    };
    check!(at("sub/g", 0) == 0, "unlinkat sub/g");
    // ext2 has no `rmdir` yet, so only `/tmp` directories go away.
    if mount == "/tmp" {
        let removed = at("m", AT_REMOVEDIR);
        check!(removed == 0, "unlinkat m returned {removed:#x}");
        check!(rmdir("sub") == 0, "rmdir sub");
        check!(chdir("..") == 0 && rmdir("w") == 0, "tidy up");
    }
    check!(chdir(mount) == 0, "leaving {work}");
    check!(cwd_is(mount).is_ok(), "the cwd after leaving w");
    Ok(())
}

/// Relative names work for every path syscall on both mounts, and resolve to
/// the same files as the absolute names.
pub fn relative_paths_on_both_mounts() -> Result<(), String> {
    let data = Data::new(0)?;
    relative_round("/tmp")?;
    relative_round("/data")?;
    data.check_clean()
}

/// A descriptor as `dirfd` overrides the working directory for relative names;
/// an absolute name overrides both; a non-directory or closed descriptor is
/// refused.
pub fn dirfd_names_ignore_the_cwd() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(mkdir("/data/d") == 0, "mkdir");
    put("/data/d/f", b"under data")?;
    put("/tmp/f", b"under tmp")?;
    check!(chdir("/tmp") == 0, "chdir");

    let dfd = open_dir("/data/d");
    check!(dfd < 16, "open dir");
    let via = |dirfd: u64, name: &str| {
        let fd = syscall(SYS_OPENAT, dirfd, cstr(name).as_ptr() as u64, O_RDONLY, 0);
        if fd >= 16 {
            return Err(fd);
        }
        let bytes = read(fd, 32);
        close(fd);
        bytes
    };
    check!(via(dfd, "f") == Ok(b"under data".to_vec()), "openat(dirfd)");
    check!(
        via(AT_FDCWD, "f") == Ok(b"under tmp".to_vec()),
        "openat(AT_FDCWD)"
    );
    check!(via(dfd, "/tmp/f") == Ok(b"under tmp".to_vec()), "absolute");
    check!(via(dfd, "../d/./f") == Ok(b"under data".to_vec()), "folded");
    check!(ino_size_at(dfd, "f", 0)?.1 == 10, "newfstatat(dirfd)");
    let itself = ino_size_at(dfd, "", AT_EMPTY_PATH)?;
    let named = ino_size_at(AT_FDCWD, "/data/d", 0)?;
    check!(itself.0 == named.0, "dirfd itself: {itself:?} vs {named:?}");

    // Directories are made and removed relative to a descriptor too; the
    // removal uses a `/tmp` one, since ext2 has no `rmdir` yet.
    let n = cstr("n");
    check!(
        syscall(SYS_MKDIRAT, dfd, n.as_ptr() as u64, 0o755, 0) == 0,
        "mkdirat"
    );
    let made = open_dir("/data/d/n");
    check!(made < 16, "mkdirat landed under the dirfd");
    close(made);
    let tmp = open_dir("/tmp");
    check!(
        syscall(SYS_MKDIRAT, tmp, n.as_ptr() as u64, 0o755, 0) == 0,
        "mkdirat on /tmp"
    );
    let removed = syscall(SYS_UNLINKAT, tmp, n.as_ptr() as u64, AT_REMOVEDIR, 0);
    check!(removed == 0, "unlinkat returned {removed:#x}");
    close(tmp);

    let file = open("/tmp/f", O_RDONLY);
    check!(via(file, "x") == Err(errno(ENOTDIR)), "a file as dirfd");
    close(file);
    check!(via(9, "x") == Err(errno(EBADF)), "a closed dirfd");
    close(dfd);
    check!(getcwd()? == "/tmp", "the cwd was disturbed");
    data.check_clean()
}

/// `fork` starts the child where the parent is, after which neither can move
/// the other; a task's exit lets go of its reference.
///
/// Only the parent makes syscalls: the harness cannot run one as a forked task
/// (its signal-delivery epilogue reads a real syscall stack), so the children
/// are driven through the kernel interface, [`task::set_cwd`], which `chdir`
/// ends in. The ABI bench's `cwd` fixture forks for real.
pub fn fork_inherits_and_isolates() -> Result<(), String> {
    let data = Data::new(0)?;
    check!(mkdir("/data/p") == 0 && mkdir("/tmp/q") == 0, "mkdir");
    check!(chdir("/data/p") == 0, "chdir");
    let parent = task::current();
    check!(task::harness::cwd_holders(parent) == 1, "one holder");

    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    check!(
        task::harness::cwd_holders(parent) == 2,
        "the child shares it"
    );
    task::harness::switch_current(child);
    check!(
        task::cwd() == "/data/p",
        "the child starts in {}",
        task::cwd()
    );
    task::set_cwd("/tmp/q");
    check!(task::harness::cwd_holders(child) == 1, "the child let go");
    task::harness::switch_current(parent);

    // The parent did not move, and its relative names still land in /data/p.
    cwd_is("/data/p")?;
    check!(task::harness::cwd_holders(parent) == 1, "the parent's own");
    put("mine", b"parent")?;
    check!(
        slurp("/data/p/mine")? == b"parent",
        "landed in the parent's cwd"
    );
    check!(chdir("..") == 0, "the parent's chdir");
    task::harness::switch_current(child);
    check!(task::cwd() == "/tmp/q", "the child kept its cwd");

    // A thread starts in its creator's directory and moves on its own.
    let thread = task::spawn_thread("cwd-thread", process::USER_STACK_TOP, 0, 0)
        .map_err(|e| format!("thread: {e}"))?;
    check!(
        task::harness::cwd_holders(child) == 2,
        "the thread shares it"
    );
    task::harness::switch_current(thread);
    check!(
        task::cwd() == "/tmp/q",
        "the thread starts in {}",
        task::cwd()
    );
    task::set_cwd("/");
    task::harness::switch_current(child);
    check!(task::cwd() == "/tmp/q", "the thread moved its creator");
    task::harness::switch_current(parent);

    // Exiting lets go: the child and its thread hold nothing afterwards.
    task::harness::finish(child, 0);
    check!(
        task::reap_child().map(|r| r.0) == Some(child),
        "reap the child"
    );
    task::harness::reset();
    check!(
        task::harness::cwd_holders(parent) == 1,
        "exits left a reference"
    );
    check!(path_call(SYS_UNLINK, "/data/p/mine", 0) == 0, "cleanup");
    data.check_clean()
}

/// `execve` names its program relative to the working directory, and a failed
/// one leaves that directory alone. (A successful `execve` keeps the directory
/// because the task, which owns it, is the same one; the ABI bench proves it
/// end to end by running `ls` in BusyBox after a `cd`.)
pub fn execve_resolves_relative_names() -> Result<(), String> {
    let data = Data::new(0)?;
    put("/data/prog.bin", b"this is not an ELF image\n")?;
    check!(chmod("/data/prog.bin", 0o755) == 0, "chmod");
    let exec = |path: &str| syscall(SYS_EXECVE, cstr(path).as_ptr() as u64, 0, 0, 0);

    check!(exec("prog.bin") == errno(ENOENT), "not in the root");
    check!(chdir("/data") == 0, "chdir");
    for path in [
        "prog.bin",
        "./prog.bin",
        "../data/prog.bin",
        "/data/prog.bin",
    ] {
        check!(
            exec(path) == errno(ENOEXEC),
            "execve({path:?}) did not reach the file"
        );
    }
    cwd_is("/data")?;
    check!(chdir("/tmp") == 0, "chdir /tmp");
    check!(exec("../data/prog.bin") == errno(ENOEXEC), "from /tmp");
    check!(exec("prog.bin") == errno(ENOENT), "not in /tmp");
    cwd_is("/tmp")?;
    check!(path_call(SYS_UNLINK, "/data/prog.bin", 0) == 0, "unlink");
    data.check_clean()
}

const ENOEXEC: u64 = 8;

/// Many short-lived processes: each round forks a child that changes directory
/// (through the kernel interface, as above) and exits, while the parent does
/// relative I/O on both mounts from directories it enters by absolute, relative
/// and `..` moves. Nothing leaks: the working-directory string is released with
/// every task, and slots, descriptors, frames, blocks and inodes return to
/// where they started.
pub fn soak_fork_chdir_relative_io() -> Result<(), String> {
    const ROUNDS: usize = 200;
    let data = Data::new(0)?;
    check!(mkdir("/data/s") == 0 && mkdir("/tmp/s") == 0, "mkdir");
    check!(chdir("/data/s") == 0, "chdir");
    let parent = task::current();
    let blocks = free_space()?;
    let frames = crate::mem::frame_stats().live();
    let slots = task::free_slots();

    for round in 0..ROUNDS {
        let child = task::spawn_fork().map_err(|e| format!("round {round}: fork {e}"))?;
        task::harness::switch_current(child);
        task::set_cwd(if round % 2 == 0 { "/tmp/s" } else { "/" });
        task::harness::switch_current(parent);

        let root = if round % 2 == 0 { "/data" } else { "/tmp" };
        let (name, body) = (format!("f{}", round % 5), pattern_bytes(round as u32, 300));
        check!(chdir(root) == 0 && chdir("s") == 0, "round {round}: chdir");
        cwd_is(&format!("{root}/s"))?;
        put(&name, &body)?;
        check!(slurp(&name)? == body, "round {round}: read back");
        check!(
            chdir("..") == 0 && chdir("s") == 0,
            "round {round}: there and back"
        );
        let to = format!("g{}", round % 3);
        check!(rename_rel(&name, &to) == 0, "round {round}: rename");
        check!(slurp(&to)? == body, "round {round}: renamed contents");
        check!(path_call(SYS_UNLINK, &to, 0) == 0, "round {round}: unlink");
        check!(
            nothing_open(data.open_files),
            "round {round}: descriptors leaked"
        );

        task::harness::finish(child, 0);
        let reaped = task::reap_child().map(|r| r.0);
        check!(reaped == Some(child), "round {round}: reap gave {reaped:?}");
        check!(
            task::harness::cwd_holders(parent) == 1,
            "round {round}: a reference leaked"
        );
    }
    check!(free_space()? == blocks, "blocks or inodes leaked");
    check!(crate::mem::frame_stats().live() == frames, "frames leaked");
    check!(task::free_slots() == slots, "task slots leaked");
    data.check_clean()
}

/// `rename(from, to)` on relative names.
fn rename_rel(from: &str, to: &str) -> u64 {
    let (from, to) = (cstr(from), cstr(to));
    syscall(SYS_RENAME, from.as_ptr() as u64, to.as_ptr() as u64, 0, 0)
}
