//! `sched_setattr` and `sched_getattr` (314, 315): the plain policies with a
//! nice value that becomes the task's stride weight, the refusals Linux makes
//! without `CAP_SYS_NICE` (here `CAP_SYS_ADMIN`), the size rules, and what a
//! child inherits. WTF calls `sched_setattr` to lower a GC marker thread's
//! priority (docs/webkit-port-plan.md section 6).

use super::*;
use crate::ipc::credentials::{self, Cred, CAP_SYS_ADMIN};
use crate::task::nice;
use crate::task::PriorityClass;

pub(super) const SETATTR: u64 = 314;
pub(super) const GETATTR: u64 = 315;
pub(super) const GETSCHEDULER: u64 = 145;

pub(super) const OTHER: u32 = 0;
pub(super) const FIFO: u32 = 1;
pub(super) const RR: u32 = 2;
pub(super) const BATCH: u32 = 3;
pub(super) const IDLE: u32 = 5;
pub(super) const DEADLINE: u32 = 6;

pub(super) const RESET_ON_FORK: u64 = 0x01;
const RECLAIM: u64 = 0x02;
pub(super) const KEEP_POLICY: u64 = 0x08;
pub(super) const KEEP_PARAMS: u64 = 0x10;
const UTIL_CLAMP_MIN: u64 = 0x20;

const E2BIG: u64 = neg(7);
const ESRCH: u64 = neg(3);

/// `struct sched_attr` as of Linux 5.3.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(super) struct Attr {
    pub(super) size: u32,
    pub(super) policy: u32,
    pub(super) flags: u64,
    pub(super) nice: i32,
    pub(super) priority: u32,
    pub(super) runtime: u64,
    pub(super) deadline: u64,
    pub(super) period: u64,
    pub(super) util_min: u32,
    pub(super) util_max: u32,
}

pub(super) const SIZE: u32 = core::mem::size_of::<Attr>() as u32;

pub(super) fn attr(policy: u32, nice: i32) -> Attr {
    Attr {
        size: SIZE,
        policy,
        nice,
        ..Attr::default()
    }
}

pub(super) fn setattr(pid: u64, attr: &Attr) -> u64 {
    sys(SETATTR, &[pid, attr as *const Attr as u64, 0])
}

/// `sched_getattr(pid, ...)` with room for `size` bytes; the result code.
fn getattr_size(pid: u64, out: &mut [u8], size: u64) -> u64 {
    sys(GETATTR, &[pid, out.as_mut_ptr() as u64, size, 0])
}

pub(super) fn getattr(pid: u64) -> Result<Attr, String> {
    let mut out = Attr::default();
    let ret = sys(
        GETATTR,
        &[pid, &mut out as *mut Attr as u64, u64::from(SIZE), 0],
    );
    check!(ret == 0, "sched_getattr returned {ret:#x}");
    Ok(out)
}

pub(super) fn me_weight() -> u16 {
    task::weight(task::current()).unwrap_or(0)
}

/// What a class-default `base` weighs at `level`, from the documented table.
pub(super) fn weight_at(level: i32, idle: bool) -> u16 {
    nice::weight(PriorityClass::Interactive.default_weight(), level, idle)
}

/// Run `body` as the kernel task with `cred` and a default scheduling state,
/// and put root and the default back whatever happens.
pub(super) fn scoped(cred: Cred, body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let reset = || {
        credentials::set(task::current(), Cred::ROOT);
        task::linuxstate::with_extras(|extras| extras.sched = Default::default());
        task::set_weight(task::current(), PriorityClass::Interactive.default_weight());
    };
    fresh()?;
    reset();
    credentials::set(task::current(), cred);
    let result = body();
    reset();
    result
}

pub(super) const USER: Cred = Cred::new(1000, 1000, 0, 0, 0);
pub(super) const ADMIN: Cred = Cred::new(1001, 1001, CAP_SYS_ADMIN, 0, 0);

/// A plain policy and a nice value are kept and read back; `sched_getscheduler`
/// follows; the weight follows the nice value (and `SCHED_IDLE` is the least).
pub fn schedattr_set_and_get_back() -> Result<(), String> {
    scoped(USER, || {
        let before = getattr(0)?;
        check!(
            before.size == SIZE && before.policy == OTHER && before.nice == 0 && before.flags == 0,
            "default attr: size {} policy {} nice {}",
            before.size,
            before.policy,
            before.nice
        );
        let normal = me_weight();
        check!(setattr(0, &attr(BATCH, 3)) == 0, "BATCH nice 3");
        let got = getattr(0)?;
        check!(
            got.policy == BATCH && got.nice == 3 && got.priority == 0,
            "read back policy {} nice {}",
            got.policy,
            got.nice
        );
        check!(
            sys(GETSCHEDULER, &[0]) == u64::from(BATCH),
            "getscheduler after BATCH"
        );
        check!(
            me_weight() == weight_at(3, false) && me_weight() <= normal,
            "weight {} for nice 3",
            me_weight()
        );
        // A larger nice value never raises the weight, and SCHED_IDLE is the least.
        check!(setattr(0, &attr(OTHER, 19)) == 0, "OTHER nice 19");
        check!(me_weight() == weight_at(19, false), "weight at nice 19");
        check!(setattr(0, &attr(IDLE, 19)) == 0, "IDLE");
        check!(
            getattr(0)?.policy == IDLE && me_weight() == task::MIN_WEIGHT,
            "IDLE weight {}",
            me_weight()
        );
        check!(sys(GETSCHEDULER, &[0]) == 5, "getscheduler after IDLE");
        // The caller's own tid names the same task as pid 0.
        let tid = task::current() as u64;
        check!(getattr(tid)?.policy == IDLE, "by tid");
        Ok(())
    })
}

/// The nice-to-weight table: exact values for the kernel task's class
/// (Interactive, base 4) and for a Normal task (base 2), monotone for every base.
pub fn schedattr_nice_maps_to_weight() -> Result<(), String> {
    scoped(ADMIN, || {
        for (level, want) in [
            (0, 4),
            (1, 3),
            (5, 1),
            (19, 1),
            (-1, 5),
            (-5, 12),
            (-20, 32),
        ] {
            check!(setattr(0, &attr(OTHER, level)) == 0, "nice {level}");
            check!(
                me_weight() == want,
                "nice {level}: weight {} want {want}",
                me_weight()
            );
        }
        for (level, want) in [(0, 2), (1, 2), (2, 1), (-1, 2), (-2, 3), (-20, 32)] {
            let got = nice::weight(2, level, false);
            check!(got == want, "Normal base, nice {level}: {got} want {want}");
        }
        for base in 1..=8u16 {
            check!(
                nice::weight(base, 0, false) == base,
                "base {base} at nice 0"
            );
            let mut last = u16::MAX;
            for level in nice::MIN_NICE..=nice::MAX_NICE {
                let weight = nice::weight(base, level, false);
                check!(
                    weight <= last && (1..=32).contains(&weight),
                    "base {base} nice {level}: {weight} after {last}"
                );
                last = weight;
            }
            check!(nice::weight(base, 0, true) == 1, "idle base {base}");
        }
        // Out of range nice values clamp, as Linux does.
        check!(setattr(0, &attr(OTHER, 100)) == 0, "nice 100");
        check!(getattr(0)?.nice == 19, "nice 100 clamps to 19");
        check!(setattr(0, &attr(OTHER, -100)) == 0, "nice -100");
        check!(getattr(0)?.nice == -20, "nice -100 clamps to -20");
        Ok(())
    })
}

/// Real-time and deadline policies: `EPERM` without privilege, `EINVAL` for
/// bad parameters whoever asks, and `EINVAL` with privilege too (LazyOS has
/// no such policy to give).
pub fn schedattr_realtime_refused() -> Result<(), String> {
    let rt = |policy, priority| Attr {
        priority,
        ..attr(policy, 0)
    };
    let dl = Attr {
        runtime: 1_000_000,
        deadline: 2_000_000,
        period: 4_000_000,
        ..attr(DEADLINE, 0)
    };
    scoped(USER, || {
        for policy in [FIFO, RR] {
            check!(
                setattr(0, &rt(policy, 10)) == EPERM,
                "user, policy {policy}"
            );
            check!(setattr(0, &rt(policy, 0)) == EINVAL, "priority 0");
            check!(setattr(0, &rt(policy, 100)) == EINVAL, "priority 100");
        }
        check!(setattr(0, &dl) == EPERM, "user, DEADLINE");
        let bad = Attr { runtime: 0, ..dl };
        check!(setattr(0, &bad) == EINVAL, "DEADLINE without runtime");
        check!(
            setattr(
                0,
                &Attr {
                    priority: 5,
                    ..attr(OTHER, 0)
                }
            ) == EINVAL,
            "OTHER with a priority"
        );
        check!(
            setattr(
                0,
                &Attr {
                    flags: RECLAIM,
                    ..attr(OTHER, 0)
                }
            ) == EINVAL,
            "RECLAIM on a fair policy"
        );
        for policy in [4, 7, 100] {
            check!(setattr(0, &attr(policy, 0)) == EINVAL, "policy {policy}");
        }
        check!(
            getattr(0)?.policy == OTHER,
            "a refused call changed the policy"
        );
        Ok(())
    })?;
    scoped(ADMIN, || {
        check!(setattr(0, &rt(FIFO, 10)) == EINVAL, "admin, FIFO");
        check!(setattr(0, &dl) == EINVAL, "admin, DEADLINE");
        check!(
            getattr(0)?.policy == OTHER,
            "admin refusal changed the policy"
        );
        Ok(())
    })
}

/// Argument checks: flags, null, negative pid, missing task, util clamp.
pub fn schedattr_argument_errors() -> Result<(), String> {
    scoped(USER, || {
        let ok = attr(OTHER, 0);
        let ptr = &ok as *const Attr as u64;
        check!(sys(SETATTR, &[0, ptr, 1]) == EINVAL, "set with flags");
        check!(sys(SETATTR, &[0, 0, 0]) == EINVAL, "set with null");
        check!(sys(SETATTR, &[u64::MAX, ptr, 0]) == EINVAL, "set pid -1");
        check!(sys(SETATTR, &[900, ptr, 0]) == ESRCH, "set absent task");
        check!(
            setattr(0, &Attr { flags: 0x80, ..ok }) == EINVAL,
            "unknown flag"
        );
        check!(
            setattr(
                0,
                &Attr {
                    flags: UTIL_CLAMP_MIN,
                    ..ok
                }
            ) == EOPNOTSUPP,
            "util clamp"
        );
        let mut out = [0u8; 64];
        let want = |code| code == EINVAL;
        check!(want(sys(GETATTR, &[0, 0, 56, 0])), "get with null");
        check!(want(getattr_size(0, &mut out, 47)), "get with size 47");
        check!(want(getattr_size(0, &mut out, 4097)), "get with size 4097");
        check!(
            want(sys(GETATTR, &[0, out.as_mut_ptr() as u64, 56, 1])),
            "get flags"
        );
        check!(
            want(sys(GETATTR, &[u64::MAX, out.as_mut_ptr() as u64, 56, 0])),
            "get pid -1"
        );
        check!(
            sys(GETATTR, &[900, out.as_mut_ptr() as u64, 56, 0]) == ESRCH,
            "get absent task"
        );
        check!(sys(GETSCHEDULER, &[900]) == ESRCH, "getscheduler absent");
        Ok(())
    })
}

/// The `sched_attr` size handshake: too small is `E2BIG` and the size the
/// kernel knows is written back; zero means the first version's; a larger
/// struct needs zero tail bytes; `sched_getattr` fills what fits and zeroes
/// a larger buffer's tail.
pub fn schedattr_size_rules() -> Result<(), String> {
    scoped(USER, || {
        // The kernel writes the size it knows back into a struct it refuses.
        let mut raw = [0u8; 56];
        raw[0..4].copy_from_slice(&47u32.to_le_bytes());
        check!(
            sys(SETATTR, &[0, raw.as_mut_ptr() as u64, 0]) == E2BIG,
            "size 47"
        );
        check!(
            u32::from_le_bytes(raw[0..4].try_into().unwrap()) == 48,
            "size not written back: {:?}",
            &raw[0..4]
        );
        raw[0..4].copy_from_slice(&0u32.to_le_bytes());
        check!(
            sys(SETATTR, &[0, raw.as_mut_ptr() as u64, 0]) == 0,
            "size 0 is the first version"
        );
        raw[0..4].copy_from_slice(&5000u32.to_le_bytes());
        check!(
            sys(SETATTR, &[0, raw.as_mut_ptr() as u64, 0]) == E2BIG,
            "size 5000"
        );

        // A 64-byte struct: the 8 bytes past this kernel's are unknown.
        let mut big = [0u8; 64];
        big[0..4].copy_from_slice(&64u32.to_le_bytes());
        check!(
            sys(SETATTR, &[0, big.as_ptr() as u64, 0]) == 0,
            "zero tail accepted"
        );
        big[60] = 1;
        check!(
            sys(SETATTR, &[0, big.as_ptr() as u64, 0]) == E2BIG,
            "non-zero tail"
        );
        check!(
            u32::from_le_bytes(big[0..4].try_into().unwrap()) == SIZE,
            "size not written back after a non-zero tail"
        );

        let mut out = [0xAAu8; 64];
        check!(getattr_size(0, &mut out, 64) == 0, "get into 64 bytes");
        check!(
            u32::from_le_bytes(out[0..4].try_into().unwrap()) == SIZE,
            "size field {:?}",
            &out[0..4]
        );
        check!(out[56..].iter().all(|&b| b == 0), "tail not zeroed");
        let mut out = [0xAAu8; 64];
        check!(getattr_size(0, &mut out, 48) == 0, "get into 48 bytes");
        check!(
            u32::from_le_bytes(out[0..4].try_into().unwrap()) == 48,
            "size field with room for 48"
        );
        check!(out[48..].iter().all(|&b| b == 0xAA), "wrote past 48");
        Ok(())
    })
}

/// What needs privilege: lowering the nice value, leaving `SCHED_IDLE`,
/// clearing `reset_on_fork`, and touching another user's task.
pub fn schedattr_privilege_rules() -> Result<(), String> {
    scoped(USER, || {
        let other = task::spawn_thread("schedattr", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        check!(setattr(0, &attr(OTHER, 5)) == 0, "raise nice to 5");
        check!(setattr(0, &attr(OTHER, 5)) == 0, "same nice again");
        check!(setattr(0, &attr(OTHER, 10)) == 0, "raise nice to 10");
        check!(setattr(0, &attr(OTHER, 4)) == EPERM, "lower nice to 4");
        check!(
            getattr(0)?.nice == 10,
            "refused call changed the nice value"
        );
        check!(setattr(0, &attr(OTHER, 0)) == EPERM, "lower nice to 0");

        check!(setattr(0, &attr(IDLE, 10)) == 0, "to IDLE");
        check!(setattr(0, &attr(OTHER, 10)) == EPERM, "leave IDLE");
        check!(setattr(0, &attr(BATCH, 10)) == EPERM, "IDLE to BATCH");
        check!(setattr(0, &attr(IDLE, 12)) == 0, "stay IDLE");

        let flagged = Attr {
            flags: RESET_ON_FORK,
            ..attr(IDLE, 12)
        };
        check!(setattr(0, &flagged) == 0, "set reset_on_fork");
        check!(
            sys(GETSCHEDULER, &[0]) == (5 | 0x4000_0000),
            "getscheduler with reset_on_fork"
        );
        check!(setattr(0, &attr(IDLE, 12)) == EPERM, "clear reset_on_fork");

        // Another user's task: refused, and it is not changed.
        credentials::set(other, Cred::new(2000, 2000, 0, 0, 0));
        let weight = task::weight(other);
        check!(
            setattr(other as u64, &attr(OTHER, 1)) == EPERM,
            "other user's task"
        );
        check!(
            task::weight(other) == weight && getattr(other as u64)?.nice == 0,
            "other task changed"
        );
        // The same user's task: allowed, and the weight is that task's.
        credentials::set(other, USER);
        check!(setattr(other as u64, &attr(OTHER, 1)) == 0, "own task");
        check!(
            task::weight(other) == Some(weight_at(1, false)) && weight != task::weight(other),
            "own task's weight {:?} (was {weight:?})",
            task::weight(other)
        );
        check!(getattr(other as u64)?.nice == 1, "own task's nice");
        check!(getattr(0)?.nice == 12, "the caller's nice moved");
        Ok(())
    })?;
    // With privilege the same calls work.
    scoped(ADMIN, || {
        check!(setattr(0, &attr(IDLE, 19)) == 0, "admin to IDLE");
        check!(
            setattr(0, &attr(OTHER, -10)) == 0,
            "admin out of IDLE, nice -10"
        );
        check!(getattr(0)?.nice == -10, "negative nice read back");
        Ok(())
    })
}
