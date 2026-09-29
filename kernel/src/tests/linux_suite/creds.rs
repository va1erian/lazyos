//! Linux credential syscalls (issue #231): `getuid` family reports the real
//! per-task credentials, and `setuid` family goes through the audited gate
//! (refusals are `-EPERM`, never a silent success).

use super::*;
use crate::ipc::credentials::{self, Cred};

const SYS_GETUID: u64 = 102;
const SYS_GETGID: u64 = 104;
const SYS_SETUID: u64 = 105;
const SYS_SETGID: u64 = 106;
const SYS_GETEUID: u64 = 107;
const SYS_GETEGID: u64 = 108;
const SYS_SETREUID: u64 = 113;
const SYS_SETRESUID: u64 = 117;
const SYS_SETRESGID: u64 = 119;

const NONE: u64 = u32::MAX as u64;

fn eperm() -> u64 {
    (-1i64) as u64
}

fn einval() -> u64 {
    (-22i64) as u64
}

/// Make the (kernel) task the caller and stamp it with `cred`.
fn become_task(cred: Cred) -> Result<(), String> {
    fresh()?;
    credentials::set(task::current(), cred);
    Ok(())
}

fn call(nr: u64, a1: u64, a2: u64, a3: u64) -> u64 {
    process::linux::dispatch_for_test(nr, a1, a2, a3)
}

fn now() -> Cred {
    credentials::of(task::current())
}

/// `getuid` and friends report the stamped ids, not a hard-coded 0.
pub fn getuid_family_reports_credentials() -> Result<(), String> {
    become_task(Cred::new(1000, 100, 0, 0, 0))?;
    for (nr, want) in [
        (SYS_GETUID, 1000),
        (SYS_GETEUID, 1000),
        (SYS_GETGID, 100),
        (SYS_GETEGID, 100),
    ] {
        let got = call(nr, 0, 0, 0);
        check!(got == want, "syscall {nr} returned {got}, want {want}");
    }
    credentials::set(task::current(), Cred::ROOT);
    check!(
        call(SYS_GETUID, 0, 0, 0) == 0,
        "root does not read as uid 0"
    );
    Ok(())
}

/// An unprivileged task cannot change ids and is told so; no-op changes
/// (to its own ids, or `-1`) succeed.
pub fn setuid_unprivileged_refused() -> Result<(), String> {
    let cred = Cred::new(1000, 100, 0, 0, 0);
    become_task(cred)?;
    let refused = [
        (SYS_SETUID, 0, 0, 0),
        (SYS_SETUID, 2000, 0, 0),
        (SYS_SETGID, 0, 0, 0),
        (SYS_SETREUID, 0, 0, 0),
        (SYS_SETRESUID, 0, 0, 0),
        (SYS_SETRESUID, 1000, 0, NONE), // split ids are unsupported
        (SYS_SETRESGID, 0, 0, 0),
    ];
    for (nr, a1, a2, a3) in refused {
        let got = call(nr, a1, a2, a3);
        check!(
            got == eperm(),
            "syscall {nr}({a1},{a2},{a3}) = {got:#x}, want -EPERM"
        );
        check!(
            now() == cred,
            "syscall {nr} changed credentials: {:?}",
            now()
        );
    }
    check!(
        call(SYS_SETUID, NONE, 0, 0) == einval(),
        "setuid(-1) not EINVAL"
    );
    for (nr, a1, a2, a3) in [
        (SYS_SETUID, 1000, 0, 0),
        (SYS_SETGID, 100, 0, 0),
        (SYS_SETRESUID, NONE, NONE, NONE),
        (SYS_SETREUID, 1000, NONE, 0x1_0000_03e8), // high bits truncate
    ] {
        let got = call(nr, a1, a2, a3);
        check!(got == 0, "no-op syscall {nr} returned {got:#x}");
    }
    check!(now() == cred, "no-op calls changed credentials");
    Ok(())
}

/// A `CAP_SETUID` holder can drop, group first; the drop is irreversible
/// and sheds capabilities.
pub fn setuid_privileged_drop_is_irreversible() -> Result<(), String> {
    become_task(Cred::ROOT)?;
    check!(call(SYS_SETGID, 100, 0, 0) == 0, "root setgid refused");
    check!(
        now().gid == 100 && now().uid == 0,
        "setgid result {:?}",
        now()
    );
    check!(now().caps == Cred::ROOT.caps, "setgid dropped capabilities");
    check!(
        call(SYS_SETRESUID, 1000, 1000, 1000) == 0,
        "root setresuid refused"
    );
    check!(
        now().uid == 1000 && now().caps == 0,
        "drop left uid/caps {:?}",
        now()
    );
    check!(
        call(SYS_GETUID, 0, 0, 0) == 1000,
        "getuid stale after setuid"
    );
    check!(
        call(SYS_SETUID, 0, 0, 0) == eperm(),
        "dropped task regained root"
    );
    check!(
        call(SYS_SETGID, 5, 0, 0) == eperm(),
        "dropped task changed gid"
    );
    Ok(())
}

/// Soak: an unprivileged task hammering every setter with pseudo-random ids
/// never changes its credentials and never gets a false success.
pub fn setuid_soak_never_widens() -> Result<(), String> {
    let cred = Cred::new(1000, 100, 0, 0, 0);
    become_task(cred)?;
    let setters = [
        SYS_SETUID,
        SYS_SETGID,
        SYS_SETREUID,
        SYS_SETRESUID,
        SYS_SETRESGID,
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for i in 0..20_000u32 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        // Bias toward the values that are legal no-ops so both outcomes run.
        let id = match state % 4 {
            0 => 1000,
            1 => 100,
            2 => NONE,
            _ => state >> 32,
        };
        let nr = setters[(i % 5) as usize];
        let group = nr == SYS_SETGID || nr == SYS_SETRESGID;
        let got = call(nr, id, id, id);
        let noop = if group { id == 100 } else { id == 1000 }
            || (id == NONE && nr != SYS_SETUID && nr != SYS_SETGID);
        check!(
            (got == 0) == noop,
            "iteration {i}: syscall {nr}({id:#x}) = {got:#x}"
        );
        check!(
            now() == cred,
            "iteration {i}: credentials changed to {:?}",
            now()
        );
    }
    Ok(())
}
