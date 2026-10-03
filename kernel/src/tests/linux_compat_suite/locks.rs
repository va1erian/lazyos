//! Advisory locks: `fcntl` record locks between processes, `flock` between
//! open file descriptions, and release when the owner goes away.

use super::*;

const FCNTL: u64 = 72;
const FLOCK: u64 = 73;
const F_GETLK: u64 = 5;
const F_SETLK: u64 = 6;
const F_RDLCK: i16 = 0;
const F_WRLCK: i16 = 1;
const F_UNLCK: i16 = 2;
const LOCK_SH: u64 = 1;
const LOCK_EX: u64 = 2;
const LOCK_NB: u64 = 4;
const LOCK_UN: u64 = 8;

/// A `struct flock` (`l_type`, `l_whence` = `SEEK_SET`, `l_start`, `l_len`).
fn flock_struct(kind: i16, start: i64, len: i64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..2].copy_from_slice(&kind.to_le_bytes());
    out[8..16].copy_from_slice(&start.to_le_bytes());
    out[16..24].copy_from_slice(&len.to_le_bytes());
    out
}

fn setlk(fd: u64, kind: i16, start: i64, len: i64) -> u64 {
    let lock = flock_struct(kind, start, len);
    sys(FCNTL, &[fd, F_SETLK, lock.as_ptr() as u64])
}

/// Open (creating) `path` read-write.
fn open(path: &str) -> Result<u64, String> {
    let name = cpath(path);
    let fd = sys(2, &[name.as_ptr() as u64, 0o102, 0o644]);
    check!((fd as i64) >= 0, "open {path}: {fd:#x}");
    Ok(fd)
}

fn forked() -> Result<usize, String> {
    task::spawn_fork().map_err(|error| format!("fork: {error}"))
}

/// Record locks conflict between processes (not within one), `F_GETLK`
/// names the holder, unlocking frees the range, and a dead owner's locks go.
/// `flock` follows the open file description.
pub fn flock_and_record_locks() -> Result<(), String> {
    fresh()?;
    let leader = forked()?;
    task::harness::switch_current(leader);
    let path = "/tmp/compat-lock";
    let fd = open(path)?;
    check!(setlk(fd, F_WRLCK, 0, 10) == 0, "first lock");
    check!(
        setlk(fd, F_WRLCK, 5, 10) == 0,
        "a process does not conflict with itself"
    );
    let other = forked()?;
    task::harness::switch_current(other);
    check!(
        setlk(fd, F_WRLCK, 0, 4) == EAGAIN,
        "a conflicting record lock was granted"
    );
    check!(
        setlk(fd, F_RDLCK, 20, 5) == 0,
        "a disjoint read lock was refused"
    );
    let mut query = flock_struct(F_WRLCK, 0, 1);
    check!(
        sys(FCNTL, &[fd, F_GETLK, query.as_mut_ptr() as u64]) == 0,
        "F_GETLK"
    );
    let kind = i16::from_le_bytes([query[0], query[1]]);
    let pid = i32::from_le_bytes(query[24..28].try_into().unwrap());
    check!(
        kind == F_WRLCK && pid == leader as i32,
        "F_GETLK reported {kind}/{pid}"
    );
    task::harness::switch_current(leader);
    check!(setlk(fd, F_UNLCK, 0, 0) == 0, "unlock all");
    task::harness::switch_current(other);
    check!(
        setlk(fd, F_WRLCK, 0, 4) == 0,
        "the freed range was not granted"
    );
    // flock: the fork shares the description, a fresh open does not.
    check!(sys(FLOCK, &[fd, LOCK_EX | LOCK_NB]) == 0, "flock EX");
    task::harness::switch_current(leader);
    check!(
        sys(FLOCK, &[fd, LOCK_EX | LOCK_NB]) == 0,
        "the same description conflicted"
    );
    let second = open(path)?;
    check!(
        sys(FLOCK, &[second, LOCK_SH | LOCK_NB]) == EAGAIN,
        "a second description got SH"
    );
    check!(sys(FLOCK, &[fd, LOCK_UN]) == 0, "flock UN");
    check!(sys(FLOCK, &[second, LOCK_SH | LOCK_NB]) == 0, "SH after UN");
    check!(
        sys(FLOCK, &[second, 99]) == EINVAL,
        "a bad flock op was accepted"
    );
    check!(sys(FLOCK, &[14, LOCK_SH]) == EBADF, "flock on a closed fd");
    // The other process exits: its record lock goes with it.
    task::harness::finish(other, 0);
    let _ = task::reap_child();
    check!(
        setlk(fd, F_WRLCK, 0, 4) == 0,
        "a dead process's lock still blocks"
    );
    sys(3, &[fd]);
    sys(3, &[second]);
    let name = cpath(path);
    sys(87, &[name.as_ptr() as u64]);
    task::harness::reset();
    check!(
        process::linux::locks_held_for_test() == 0,
        "locks outlived their owners"
    );
    Ok(())
}

/// Many lock/unlock rounds over overlapping ranges from two processes: a
/// grant never overlaps the other process's write lock, and the table ends
/// empty.
pub fn locks_soak() -> Result<(), String> {
    fresh()?;
    let first = forked()?;
    task::harness::switch_current(first);
    let path = "/tmp/compat-lock-soak";
    let fd = open(path)?;
    let second = forked()?;
    let owners = [first, second];
    let mut held: [Option<(i64, i64)>; 2] = [None, None];
    for round in 0..2000i64 {
        let who = (round % 2) as usize;
        task::harness::switch_current(owners[who]);
        let start = (round * 7) % 50;
        let len = round % 9 + 1;
        // Drop this process's previous lock first, so the model stays exact.
        setlk(fd, F_UNLCK, 0, 0);
        held[who] = None;
        let got = setlk(fd, F_WRLCK, start, len);
        let blocked = held[1 - who].is_some_and(|(s, l)| s < start + len && start < s + l);
        if blocked {
            check!(
                got == EAGAIN,
                "round {round}: an overlapping lock was granted"
            );
        } else {
            check!(
                got == 0,
                "round {round}: a free range was refused ({got:#x})"
            );
            held[who] = Some((start, len));
        }
        if round % 7 == 0 {
            setlk(fd, F_UNLCK, 0, 0);
            held[who] = None;
        }
    }
    for (who, &slot) in owners.iter().enumerate() {
        task::harness::switch_current(slot);
        setlk(fd, F_UNLCK, 0, 0);
        held[who] = None;
    }
    check!(
        process::linux::locks_held_for_test() == 0,
        "{} locks left",
        process::linux::locks_held_for_test()
    );
    sys(3, &[fd]);
    let name = cpath(path);
    sys(87, &[name.as_ptr() as u64]);
    task::harness::reset();
    Ok(())
}
