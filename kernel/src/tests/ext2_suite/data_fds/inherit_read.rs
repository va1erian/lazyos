//! The read side of a shell redirection and redirections onto the ext2
//! volume (issue #330): `prog <file` hands a forked child a descriptor 0 that
//! shares its offset with the parent's copy, and `prog >/data/f` (a file on a
//! persistent ext2 volume, like `ls > f` in a home directory, not a `/tmp`
//! snapshot) lands every byte the forked child writes through the inherited
//! descriptor.

use super::inherit::{as_child, live_bytes};
use super::*;

const SYS_DUP2: u64 = 33;

/// What the `<` input file holds.
const INPUT: &[u8] = b"line one\nline two\nline three\n";

fn dup2(old: u64, new: u64) -> u64 {
    syscall(SYS_DUP2, old, new, 0, 0)
}

/// Move `fd` onto `target` as a shell does (`open` may already have
/// returned `target` when the harness task has it closed).
fn move_onto(fd: u64, target: u64) -> Result<(), String> {
    if fd != target {
        check!(dup2(fd, target) == target, "dup2({fd}, {target})");
        check!(close(fd) == 0, "close({fd})");
    }
    Ok(())
}

/// Read `fd` to its end, in small chunks so the offset moves many times.
fn read_to_end(fd: u64) -> Result<Vec<u8>, String> {
    let mut all = Vec::new();
    loop {
        let chunk = read(fd, 5).map_err(|code| format!("read({fd}) returned {code:#x}"))?;
        if chunk.is_empty() {
            return Ok(all);
        }
        all.extend_from_slice(&chunk);
    }
}

/// `prog <path` with a forked child: the parent opens `path` read-only onto
/// descriptor 0, the child (after the exec step) reads the first line, and
/// the parent's copy continues where the child stopped and reaches EOF.
fn stdin_redirect(parent: usize, path: &str) -> Result<(), String> {
    put(path, INPUT)?;
    let fd = open(path, O_RDONLY);
    check!(fd < 16, "open({path}) returned {fd:#x}");
    // Keep the harness's own descriptor 0 (if any) to restore afterwards.
    let saved = if fd == 0 {
        u64::MAX
    } else {
        syscall(SYS_DUP, 0, 0, 0, 0)
    };
    move_onto(fd, 0)?;

    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    let first = as_child(parent, child, || {
        process::linux::close_cloexec_fds(); // the `execve` step
        read(0, 9).map_err(|code| format!("{path}: the child's read returned {code:#x}"))
    })?;
    task::harness::finish(child, 0);
    check!(task::reap_child().map(|r| r.0) == Some(child), "reap");

    check!(first == b"line one\n", "{path}: the child read {first:?}");
    check!(
        lseek(0, 0, SEEK_CUR) == 9,
        "{path}: the parent's offset did not follow the child's read"
    );
    let rest = read_to_end(0)?;
    check!(rest == &INPUT[9..], "{path}: the parent then read {rest:?}");
    check!(close(0) == 0, "{path}: close 0");
    if saved < 16 {
        check!(dup2(saved, 0) == 0 && close(saved) == 0, "restore 0");
    }
    check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path}");
    Ok(())
}

/// `prog >path` then `prog >>path` on the ext2 volume: a forked child writes
/// through the inherited descriptor 1, the file holds every byte in order,
/// and the second (append) run lands after the first.
fn ext2_write_redirect(parent: usize, path: &str) -> Result<(), String> {
    let mut want = Vec::new();
    for (round, flags) in [(0, O_TRUNC), (1, O_APPEND)] {
        let fd = open(path, O_CREAT | O_WRONLY | flags);
        check!(fd < 16, "open({path}) returned {fd:#x}");
        let saved = if fd == 1 {
            u64::MAX
        } else {
            syscall(SYS_DUP, 1, 0, 0, 0)
        };
        move_onto(fd, 1)?;
        let line = format!("listing {round}\n");
        let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
        as_child(parent, child, || {
            process::linux::close_cloexec_fds();
            let ret = write(1, line.as_bytes());
            check!(
                ret == line.len() as u64,
                "{path}: the child's write returned {ret:#x}"
            );
            Ok(())
        })?;
        task::harness::finish(child, 0);
        check!(task::reap_child().map(|r| r.0) == Some(child), "reap");
        want.extend_from_slice(line.as_bytes());
        check!(close(1) == 0, "{path}: close 1");
        if saved < 16 {
            check!(dup2(saved, 1) == 1 && close(saved) == 0, "restore 1");
        }
        let got = slurp(path)?;
        check!(got == want, "{path} after round {round}: {got:?}");
    }
    check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path}");
    Ok(())
}

/// `prog <file` from `/tmp` and from the ext2 volume, and `prog >f` /
/// `prog >>f` on the ext2 volume, each through a forked child.
pub fn fork_inherits_stdin_and_ext2_redirects() -> Result<(), String> {
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();
    stdin_redirect(parent, "/tmp/in")?;
    stdin_redirect(parent, "/data/in")?;
    ext2_write_redirect(parent, "/data/f")?;
    check!(task::free_slots() == slots, "task slots leaked");
    data.check_clean()
}

/// Many `<` children: each round a forked child reads one line of a `/data`
/// file through an inherited descriptor 0 and the parent reads the rest.
/// Task slots, frames and heap bytes return to where they started.
pub fn soak_inherited_stdin() -> Result<(), String> {
    const ROUNDS: usize = 150;
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();
    let frames = crate::mem::frame_stats().live();
    let bytes = live_bytes();
    for round in 0..ROUNDS {
        let path = if round % 2 == 0 {
            "/data/in"
        } else {
            "/tmp/in"
        };
        stdin_redirect(parent, path).map_err(|e| format!("round {round}: {e}"))?;
    }
    check!(task::free_slots() == slots, "task slots leaked");
    check!(crate::mem::frame_stats().live() == frames, "frames leaked");
    let now = live_bytes();
    check!(
        now < bytes + 16 * 1024,
        "heap grew by {} bytes over {ROUNDS} rounds",
        now.saturating_sub(bytes)
    );
    data.check_clean()
}
