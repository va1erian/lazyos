//! `sched_setattr` flags, inheritance and a long randomized run against a
//! model of the rules (helpers in `schedattr.rs`).

use super::schedattr::*;
use super::*;
use crate::mem::vma::{self, Kind, Prot};
use crate::task::PriorityClass;

/// `KEEP_POLICY` and `KEEP_PARAMS` change only the other half.
pub fn schedattr_keep_flags() -> Result<(), String> {
    scoped(ADMIN, || {
        check!(setattr(0, &attr(BATCH, 7)) == 0, "BATCH nice 7");
        let params = Attr {
            flags: KEEP_POLICY,
            ..attr(OTHER, 2)
        };
        check!(setattr(0, &params) == 0, "keep policy");
        let got = getattr(0)?;
        check!(
            got.policy == BATCH && got.nice == 2,
            "policy {} nice {}",
            got.policy,
            got.nice
        );
        let policy = Attr {
            flags: KEEP_PARAMS,
            ..attr(IDLE, 15)
        };
        check!(setattr(0, &policy) == 0, "keep params");
        let got = getattr(0)?;
        check!(
            got.policy == IDLE && got.nice == 2,
            "policy {} nice {}",
            got.policy,
            got.nice
        );
        Ok(())
    })
}

/// `SCHED_IDLE` takes no nice value, as in Linux: an unprivileged task at
/// nice 10 may go IDLE with the zeroed nice a `chrt --idle` sends, and
/// `sched_getattr` still reports 10; asking for nice -5 changes nothing
/// either; a privileged one in IDLE keeps its nice value too.
pub fn schedattr_idle_ignores_nice() -> Result<(), String> {
    scoped(USER, || {
        check!(setattr(0, &attr(OTHER, 10)) == 0, "nice 10");
        check!(setattr(0, &attr(IDLE, 0)) == 0, "IDLE with nice 0");
        let got = getattr(0)?;
        check!(
            got.policy == IDLE && got.nice == 10,
            "policy {} nice {} after IDLE with nice 0",
            got.policy,
            got.nice
        );
        check!(setattr(0, &attr(IDLE, -5)) == 0, "IDLE with nice -5");
        check!(getattr(0)?.nice == 10, "IDLE with nice -5 moved the nice");
        check!(me_weight() == task::MIN_WEIGHT, "IDLE weight {}", me_weight());
        // Leaving IDLE is still privileged, and a refusal changes nothing.
        check!(setattr(0, &attr(OTHER, 10)) == EPERM, "leave IDLE");
        Ok(())
    })?;
    scoped(ADMIN, || {
        check!(setattr(0, &attr(BATCH, -3)) == 0, "BATCH nice -3");
        check!(setattr(0, &attr(IDLE, 7)) == 0, "admin to IDLE");
        check!(getattr(0)?.nice == -3, "admin IDLE took nice 7");
        check!(setattr(0, &attr(OTHER, 7)) == 0, "admin out of IDLE");
        check!(getattr(0)?.nice == 7, "nice after leaving IDLE");
        Ok(())
    })
}

/// `sched_getattr` writes `min(size, 56)` bytes, as Linux does: a 56-byte
/// buffer in the last bytes of a mapping with nothing mapped after it is
/// valid for `size = 4096`, and a buffer one byte too far in is `EFAULT`.
pub fn schedattr_getattr_end_of_mapping() -> Result<(), String> {
    const PAGE: u64 = 4096;
    let base = crate::process::linux::MMAP_BASE + 0x4200_0000;
    let table = mem::kernel_table();
    scoped(USER, || {
        vma::insert(table, base, base + PAGE, Prot::READ | Prot::WRITE, Kind::Anon);
        let outcome = (|| {
            let at = base + PAGE - u64::from(SIZE);
            for size in [u64::from(SIZE), 128, PAGE] {
                let got = sys_checked(GETATTR, &[0, at, size, 0]);
                check!(got == 0, "size {size} at the end of a mapping: {got:#x}");
                let written = crate::user_ptr::try_read::<u32>(at).map_err(|_| "read back")?;
                check!(written == SIZE, "size field {written} for size {size}");
            }
            // The first version's 48 bytes fit in the last 48.
            let got = sys_checked(GETATTR, &[0, base + PAGE - 48, 48, 0]);
            check!(got == 0, "size 48 in the last 48 bytes: {got:#x}");
            // One byte further and the 56 bytes the call writes leave the mapping.
            let got = sys_checked(GETATTR, &[0, at + 1, PAGE, 0]);
            check!(got == EFAULT, "56 bytes over the end: {got:#x}");
            Ok(())
        })();
        mem::unmap_range(table, base, base + PAGE);
        vma::remove(table, base, base + PAGE);
        outcome
    })
}

/// A thread and a forked process start with their creator's policy, nice
/// value and weight; with `reset_on_fork` a negative nice value is lost and
/// the flag is not passed on.
pub fn schedattr_children_inherit() -> Result<(), String> {
    scoped(ADMIN, || {
        check!(setattr(0, &attr(BATCH, 1)) == 0, "BATCH nice 1");
        let thread = task::spawn_thread("schedattr", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        let child = task::spawn_fork().map_err(|error| format!("fork: {error}"))?;
        for (what, slot) in [("thread", thread), ("fork", child)] {
            let got = getattr(slot as u64)?;
            check!(
                got.policy == BATCH && got.nice == 1,
                "{what}: policy {} nice {}",
                got.policy,
                got.nice
            );
            check!(
                task::weight(slot) == Some(me_weight()),
                "{what}: weight {:?} want {}",
                task::weight(slot),
                me_weight()
            );
        }

        let flagged = Attr {
            flags: RESET_ON_FORK,
            ..attr(OTHER, -5)
        };
        check!(setattr(0, &flagged) == 0, "reset_on_fork, nice -5");
        let thread = task::spawn_thread("schedattr", process::USER_STACK_TOP, 0, 0)
            .map_err(|error| format!("thread: {error}"))?;
        let got = getattr(thread as u64)?;
        check!(
            got.nice == 0 && got.flags == 0,
            "reset child: nice {} flags {}",
            got.nice,
            got.flags
        );
        check!(
            task::weight(thread) == Some(PriorityClass::Interactive.default_weight()),
            "reset child's weight {:?}",
            task::weight(thread)
        );
        check!(getattr(0)?.nice == -5, "the parent lost its nice value");
        Ok(())
    })
}

/// One xorshift step.
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

/// Thousands of random calls against a model of the rules: an admin sets
/// anything and reads it back with the matching weight; a user's calls
/// succeed exactly when the model says (never lowering the nice value,
/// never leaving IDLE), and a refused call changes nothing.
pub fn schedattr_soak() -> Result<(), String> {
    let mut seed = 0x5ced_a77d_0000_0001u64;
    scoped(ADMIN, || {
        // SCHED_IDLE keeps the nice value the task had (Linux ignores the
        // attr's), so the model carries it through.
        let mut stored = 0i32;
        for round in 0..6000u32 {
            let policy = [OTHER, BATCH, IDLE][(next(&mut seed) % 3) as usize];
            let level = (next(&mut seed) % 40) as i32 - 20;
            check!(setattr(0, &attr(policy, level)) == 0, "round {round}: set");
            if policy != IDLE {
                stored = level;
            }
            let got = getattr(0)?;
            check!(
                got.policy == policy && got.nice == stored,
                "round {round}: read back {} {} want {policy} {stored}",
                got.policy,
                got.nice
            );
            check!(
                me_weight() == weight_at(stored, policy == IDLE),
                "round {round}: weight {} for nice {stored} policy {policy}",
                me_weight()
            );
        }
        Ok(())
    })?;
    scoped(USER, || {
        let (mut policy, mut level) = (OTHER, 0i32);
        for round in 0..6000u32 {
            let ask_policy = [OTHER, BATCH, IDLE][(next(&mut seed) % 3) as usize];
            let ask_level = (next(&mut seed) % 40) as i32 - 20;
            // IDLE neither raises nor applies the asked nice value; the fair
            // policies must not lower it or leave IDLE.
            let allowed = ask_policy == IDLE || (ask_level >= level && policy != IDLE);
            let got = setattr(0, &attr(ask_policy, ask_level));
            check!(
                (got == 0) == allowed && (allowed || got == EPERM),
                "round {round}: ({policy},{level}) -> ({ask_policy},{ask_level}) returned {got:#x}"
            );
            if allowed {
                policy = ask_policy;
                if ask_policy != IDLE {
                    level = ask_level;
                }
            }
            let now = getattr(0)?;
            check!(
                now.policy == policy && now.nice == level,
                "round {round}: state {} {} want {policy} {level}",
                now.policy,
                now.nice
            );
            check!(
                me_weight() == weight_at(level, policy == IDLE),
                "round {round}: weight {}",
                me_weight()
            );
        }
        Ok(())
    })
}
