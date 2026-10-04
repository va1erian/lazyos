//! A snapshot descriptor (`/tmp`) is one open file description: `dup`, `dup2`,
//! `fcntl(F_DUPFD)`, `fork` and `execve` share its offset and what its open
//! recorded, so a shell's `prog >/tmp/out 2>&1` and `prog >>/tmp/out` land
//! every write of the program in order. Before, the copies had their own
//! offsets (stderr overwrote stdout) and a forked child had no record of the
//! open, so its writes failed with `-EBADF` and the file stayed empty.

use super::*;

const SYS_WRITEV: u64 = 20;
const SYS_DUP2: u64 = 33;
const F_DUPFD_CLOEXEC: u64 = 1030;
const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;

fn dup2(old: u64, new: u64) -> u64 {
    syscall(SYS_DUP2, old, new, 0, 0)
}

/// `writev(fd, chunks)`.
fn writev(fd: u64, chunks: &[&[u8]]) -> u64 {
    let words: Vec<u64> = chunks
        .iter()
        .flat_map(|chunk| [chunk.as_ptr() as u64, chunk.len() as u64])
        .collect();
    syscall(
        SYS_WRITEV,
        fd,
        words.as_ptr() as u64,
        chunks.len() as u64,
        0,
    )
}

/// The `st_mode` `fstat` reports for `fd`.
fn fstat_mode(fd: u64) -> Result<u32, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "fstat({fd}) returned {ret:#x}");
    Ok(u32::from_le_bytes(stat[24..28].try_into().unwrap()))
}

/// Write all of `bytes` through `fd`, or say what came back.
fn write_all(fd: u64, bytes: &[u8]) -> Result<(), String> {
    let ret = write(fd, bytes);
    check!(
        ret == bytes.len() as u64,
        "write({fd}, {} bytes) returned {ret:#x}",
        bytes.len()
    );
    Ok(())
}

/// Run `body` as the forked task `child`, then return to `parent`. The child
/// is native while it runs: a Linux task's syscall return would deliver
/// signals from a kernel stack the harness never set up.
pub(super) fn as_child<T>(
    parent: usize,
    child: usize,
    body: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    task::harness::switch_current(child);
    task::harness::set_kind(child, task::Kind::Native);
    let result = body();
    task::harness::set_kind(child, task::Kind::Linux);
    task::harness::switch_current(parent);
    result
}

/// Bytes the heap and the slab allocator have handed out.
pub(super) fn live_bytes() -> usize {
    crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used
}

/// `dup`, `dup2` and `F_DUPFD` copies of one `/tmp` descriptor write at one
/// advancing offset (`>f 2>&1`), seek together, and `fstat` the file the open
/// named through any of them.
pub fn dups_share_one_description() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/tmp/r", O_CREAT | O_TRUNC | O_WRONLY);
    check!(
        task::fd_kind(fd as usize) == task::FdKind::File,
        "a /tmp file is no longer a snapshot descriptor"
    );
    check!(dup2(fd, 9) == 9, "dup2");
    let dup = syscall(SYS_DUP, fd, 0, 0, 0);
    let fcntl_dup = syscall(SYS_FCNTL, 9, 0, 11, 0); // F_DUPFD
    check!(
        dup < 16 && fcntl_dup == 11,
        "dup {dup:#x}, F_DUPFD {fcntl_dup:#x}"
    );

    let lines: [(u64, &[u8]); 5] = [
        (fd, b"out1\n"),
        (9, b"err1\n"),
        (dup, b"out2\n"),
        (fcntl_dup, b"err2\n"),
        (9, b"err3\n"),
    ];
    let mut want = Vec::new();
    for (to, line) in lines {
        write_all(to, line)?;
        want.extend_from_slice(line);
    }
    for each in [fd, 9, dup, fcntl_dup] {
        check!(
            lseek(each, 0, SEEK_CUR) == want.len() as u64,
            "descriptor {each} has its own offset"
        );
        check!(fstat_size(each)? == want.len() as u64, "fstat({each}) size");
        let mode = fstat_mode(each)?;
        check!(mode & S_IFMT == S_IFREG, "fstat({each}) mode {mode:#o}");
    }
    check!(lseek(9, 0, SEEK_SET) == 0, "rewind");
    check!(lseek(fd, 0, SEEK_CUR) == 0, "a seek moved only one copy");
    for each in [fd, 9, dup, fcntl_dup] {
        check!(close(each) == 0, "close({each})");
    }
    check!(slurp("/tmp/r")? == want, "the file differs");
    check!(path_call(SYS_UNLINK, "/tmp/r", 0) == 0, "unlink");
    data.check_clean()
}

/// `prog >/tmp/out 2>&1` as a shell runs it: the parent opens the file and
/// `dup2`s it onto two descriptors (with a close-on-exec copy beside them),
/// forks, and the child execs (closing the marked copy) and writes with
/// `write` and `writev` on both. Every byte lands, in order, after what the
/// parent wrote first, and the parent's offset follows the child's. The same
/// for an `O_APPEND` file and for `/dev/null`.
pub fn fork_and_exec_inherit_a_redirect() -> Result<(), String> {
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();

    let fd = open("/tmp/out", O_CREAT | O_TRUNC | O_WRONLY);
    check!(dup2(fd, 5) == 5 && dup2(5, 6) == 6, "dup2 onto 5 and 6");
    let marked = syscall(SYS_FCNTL, fd, F_DUPFD_CLOEXEC, 7, 0);
    check!(marked == 7, "F_DUPFD_CLOEXEC returned {marked:#x}");
    check!(close(fd) == 0, "close the original");
    write_all(5, b"parent\n")?;

    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    as_child(parent, child, || {
        let closed = process::linux::close_cloexec_fds(); // the `execve` step
        check!(
            closed == 1 && task::fd_kind(7) == task::FdKind::Closed,
            "exec closed {closed} descriptors"
        );
        check!(lseek(5, 0, SEEK_CUR) == 7, "the child starts elsewhere");
        write_all(5, b"child:write\n")?;
        let ret = writev(5, &[b"child:", b"writev\n"]);
        check!(
            ret == 13,
            "writev through an inherited descriptor: {ret:#x}"
        );
        write_all(6, b"child:stderr\n")?;
        let mode = fstat_mode(6)?;
        check!(mode & S_IFMT == S_IFREG, "the child's fstat mode {mode:#o}");
        Ok(())
    })?;
    task::harness::finish(child, 0);
    check!(task::reap_child().map(|r| r.0) == Some(child), "reap");

    let mut want = b"parent\nchild:write\nchild:writev\nchild:stderr\n".to_vec();
    check!(
        lseek(6, 0, SEEK_CUR) == want.len() as u64,
        "the parent's offset did not follow the child's"
    );
    write_all(6, b"parent:after\n")?;
    want.extend_from_slice(b"parent:after\n");
    for each in [5, 6, 7] {
        check!(close(each) == 0, "close({each})");
    }
    check!(slurp("/tmp/out")? == want, "after `>`: the file differs");

    append_and_null_inherit(parent)?;
    for path in ["/tmp/out", "/tmp/log"] {
        check!(path_call(SYS_UNLINK, path, 0) == 0, "unlink {path}");
    }
    check!(task::free_slots() == slots, "task slots leaked");
    data.check_clean()
}

/// `>>/tmp/log` and `>/dev/null` inherited by a forked child.
fn append_and_null_inherit(parent: usize) -> Result<(), String> {
    let log = open("/tmp/log", O_CREAT | O_TRUNC | O_WRONLY | O_APPEND);
    let null = open("/dev/null", O_WRONLY);
    check!(log < 16 && null < 16, "open: {log:#x} {null:#x}");
    write_all(log, b"a")?;
    let child = task::spawn_fork().map_err(|e| format!("fork: {e}"))?;
    as_child(parent, child, || {
        process::linux::close_cloexec_fds();
        write_all(log, b"b")?;
        write_all(null, b"discarded")
    })?;
    task::harness::finish(child, 0);
    check!(task::reap_child().map(|r| r.0) == Some(child), "reap");
    check!(lseek(log, 0, SEEK_SET) == 0, "rewind the log");
    write_all(log, b"c")?; // O_APPEND: lands at EOF regardless
    check!(close(log) == 0 && close(null) == 0, "close");
    check!(slurp("/tmp/log")? == b"abc", "after `>>`: the file differs");
    Ok(())
}

/// Many redirected children: each round the parent opens a large `/tmp` file
/// (alternately `O_APPEND`), `dup2`s it, writes, forks, and the child execs
/// and writes through the inherited copy. Every round's file is exact, and
/// task slots, frames and heap bytes return to where they started: the shared
/// descriptions and their snapshots are freed with their last descriptor.
pub fn soak_inherited_redirects() -> Result<(), String> {
    const ROUNDS: usize = 200;
    const HEAD: usize = 64 * 1024;
    let data = Data::new(0)?;
    let parent = task::current();
    let slots = task::free_slots();
    let frames = crate::mem::frame_stats().live();
    let bytes = live_bytes();

    for round in 0..ROUNDS {
        let append = if round % 2 == 0 { O_APPEND } else { 0 };
        let fd = open("/tmp/soak", O_CREAT | O_TRUNC | O_WRONLY | append);
        check!(dup2(fd, 6) == 6, "round {round}: dup2");
        let mut want = pattern_bytes(round as u32, HEAD);
        write_all(fd, &want)?;

        let child = task::spawn_fork().map_err(|e| format!("round {round}: fork {e}"))?;
        let tail = format!("round {round}\n");
        as_child(parent, child, || {
            process::linux::close_cloexec_fds();
            let (a, b) = tail.as_bytes().split_at(3);
            let ret = writev(6, &[a, b]);
            check!(ret == tail.len() as u64, "round {round}: writev {ret:#x}");
            Ok(())
        })?;
        task::harness::finish(child, 0);
        check!(
            task::reap_child().map(|r| r.0) == Some(child),
            "round {round}: reap"
        );

        want.extend_from_slice(tail.as_bytes());
        check!(
            lseek(fd, 0, SEEK_CUR) == want.len() as u64,
            "round {round}: offset"
        );
        check!(close(fd) == 0 && close(6) == 0, "round {round}: close");
        check!(slurp("/tmp/soak")? == want, "round {round}: contents");
        check!(
            path_call(SYS_UNLINK, "/tmp/soak", 0) == 0,
            "round {round}: unlink"
        );
    }
    check!(task::free_slots() == slots, "task slots leaked");
    check!(crate::mem::frame_stats().live() == frames, "frames leaked");
    // Allow allocator caches to settle, but not one leaked snapshot.
    let now = live_bytes();
    check!(
        now < bytes + HEAD,
        "heap grew by {} bytes over {ROUNDS} rounds",
        now.saturating_sub(bytes)
    );
    data.check_clean()
}
