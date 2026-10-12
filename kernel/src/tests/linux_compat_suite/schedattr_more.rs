//! `sched_setattr` flags, inheritance and a long randomized run against a
//! model of the rules (helpers in `schedattr.rs`).

use super::schedattr::*;
use super::*;
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
        for round in 0..6000u32 {
            let policy = [OTHER, BATCH, IDLE][(next(&mut seed) % 3) as usize];
            let level = (next(&mut seed) % 40) as i32 - 20;
            check!(setattr(0, &attr(policy, level)) == 0, "round {round}: set");
            let got = getattr(0)?;
            check!(
                got.policy == policy && got.nice == level,
                "round {round}: read back {} {} want {policy} {level}",
                got.policy,
                got.nice
            );
            check!(
                me_weight() == weight_at(level, policy == IDLE),
                "round {round}: weight {} for nice {level} policy {policy}",
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
            let allowed = ask_level >= level && (policy != IDLE || ask_policy == IDLE);
            let got = setattr(0, &attr(ask_policy, ask_level));
            check!(
                (got == 0) == allowed && (allowed || got == EPERM),
                "round {round}: ({policy},{level}) -> ({ask_policy},{ask_level}) returned {got:#x}"
            );
            if allowed {
                (policy, level) = (ask_policy, ask_level);
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
