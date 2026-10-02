//! `power` (native syscall 21) and the shutdown watchdog (docs/shutdown.md):
//! the gate on every op, the one-way arm, and the expiry that fires exactly
//! once. The stops themselves are stubbed under `lazyos_tests` (they would end
//! the test VM), so what is proven is everything up to them.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::process::power::{self, watchdog};

const EPERM: u64 = (-1i64) as u64;
const EINVAL: u64 = (-22i64) as u64;
const POWER: u64 = 21;

/// A clean slate: the kernel task current, root credentials, no watchdog.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::reset_for_task(task::KERNEL_TASK);
    watchdog::reset_for_tests();
}

fn call(op: u64, arg: u64) -> u64 {
    process::dispatch_for_test(POWER, op, arg, 0)
}

/// Without `CAP_SYS_ADMIN`, arming is refused like a stop, before the argument
/// is looked at, and leaves the watchdog disarmed.
pub fn arm_is_capability_gated() -> Result<(), String> {
    fresh();
    let me = task::current();
    credentials::set(me, Cred::new(1000, 1000, 0, 0, 7));
    let valid = call(power::ARM_WATCHDOG, power::SHUTDOWN);
    let bogus = call(power::ARM_WATCHDOG, 99);
    credentials::reset_for_task(me);
    check!(valid == EPERM, "an unprivileged arm -> {valid:#x}");
    check!(
        bogus == EPERM,
        "the gate leaked the argument check: {bogus:#x}"
    );
    check!(
        watchdog::armed().is_none(),
        "a refused arm armed the watchdog"
    );
    Ok(())
}

/// Root may arm only for a real stop: any other argument is `EINVAL`.
pub fn arm_validates_the_stop() -> Result<(), String> {
    fresh();
    for arg in [2, 3, 99, u64::MAX] {
        let result = call(power::ARM_WATCHDOG, arg);
        check!(result == EINVAL, "arm({arg}) -> {result:#x}");
    }
    check!(watchdog::armed().is_none(), "a bad arm armed the watchdog");
    check!(
        call(power::ARM_WATCHDOG, power::REBOOT) == 0,
        "root could not arm for a reboot"
    );
    let (deadline, op) = watchdog::armed().ok_or("the syscall did not arm")?;
    check!(op == power::REBOOT, "armed for op {op}");
    check!(
        deadline >= task::ticks() && deadline <= task::ticks() + watchdog::TIMEOUT_TICKS,
        "deadline {deadline} is not within the timeout of now"
    );
    watchdog::reset_for_tests();
    Ok(())
}

/// The first arm wins: a second (even for another stop, even later) keeps
/// the first deadline and op, so a stalled supervisor cannot postpone it.
pub fn first_arm_wins() -> Result<(), String> {
    fresh();
    check!(watchdog::arm(power::SHUTDOWN, 100), "the first arm failed");
    check!(
        !watchdog::arm(power::REBOOT, 5_000),
        "a second arm reported success"
    );
    let armed = watchdog::armed();
    check!(
        armed == Some((100 + watchdog::TIMEOUT_TICKS, power::SHUTDOWN)),
        "the second arm moved the watchdog: {armed:?}"
    );
    watchdog::reset_for_tests();
    Ok(())
}

/// Nothing fires before the deadline; at it, the op comes out once, and the
/// fired watchdog cannot be armed again.
pub fn expiry_fires_once() -> Result<(), String> {
    fresh();
    watchdog::arm(power::REBOOT, 10);
    let deadline = 10 + watchdog::TIMEOUT_TICKS;
    check!(
        watchdog::take_expired(deadline - 1).is_none(),
        "fired a tick early"
    );
    check!(
        watchdog::take_expired(deadline) == Some(power::REBOOT),
        "did not fire at the deadline"
    );
    check!(
        watchdog::take_expired(deadline + 1).is_none(),
        "fired twice"
    );
    check!(
        !watchdog::arm(power::SHUTDOWN, deadline),
        "a fired watchdog was armed again"
    );
    check!(
        watchdog::armed().is_none(),
        "a fired watchdog still reads armed"
    );
    watchdog::reset_for_tests();
    Ok(())
}

/// An arm at the end of time cannot overflow the packed deadline.
pub fn deadline_saturates() -> Result<(), String> {
    fresh();
    check!(
        watchdog::arm(power::SHUTDOWN, u64::MAX),
        "arm at u64::MAX failed"
    );
    let (deadline, op) = watchdog::armed().ok_or("not armed")?;
    check!(op == power::SHUTDOWN, "the op was corrupted: {op}");
    check!(deadline > 0, "the deadline wrapped to {deadline}");
    check!(
        watchdog::take_expired(deadline - 1).is_none(),
        "fired early at the clamp"
    );
    watchdog::reset_for_tests();
    Ok(())
}

/// Two stop requests in a row are safe (the second is answered like the first
/// in the stubbed build; a real stop never returns), and a watchdog armed in
/// between is not disturbed by them.
pub fn repeated_requests_are_safe() -> Result<(), String> {
    fresh();
    check!(call(power::ARM_WATCHDOG, power::SHUTDOWN) == 0, "arm");
    let before = watchdog::armed();
    for op in [power::SHUTDOWN, power::SHUTDOWN, power::REBOOT] {
        check!(call(op, 0) == 0, "stop {op} was refused for root");
    }
    check!(
        watchdog::armed() == before,
        "a stop request changed the watchdog"
    );
    watchdog::reset_for_tests();
    Ok(())
}

/// Soak: many arm/expire generations at pseudo-random ticks never fire early,
/// always fire exactly once at the deadline, and never let a late arm move it.
pub fn soak_arm_expire_generations() -> Result<(), String> {
    fresh();
    let mut seed = 0x9e37_79b9u64;
    for generation in 0..20_000u64 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let now = seed >> 24;
        let op = generation & 1;
        check!(
            watchdog::arm(op, now),
            "generation {generation}: arm failed"
        );
        let deadline = now + watchdog::TIMEOUT_TICKS;
        check!(
            !watchdog::arm(op ^ 1, now + (seed & 0xff)),
            "generation {generation}: a late arm succeeded"
        );
        check!(
            watchdog::take_expired(deadline - 1 - (seed & 0x3f)).is_none(),
            "generation {generation}: fired early"
        );
        check!(
            watchdog::take_expired(deadline + (seed & 0x7)) == Some(op),
            "generation {generation}: did not fire with its own op"
        );
        check!(
            watchdog::take_expired(deadline + 100).is_none(),
            "generation {generation}: fired twice"
        );
        watchdog::reset_for_tests();
    }
    Ok(())
}

/// Soak: thousands of gated calls from an unprivileged task leave no trace;
/// the watchdog is never armed by a refused call.
pub fn soak_refused_calls() -> Result<(), String> {
    fresh();
    let me = task::current();
    credentials::set(me, Cred::new(1000, 1000, 0, 0, 7));
    let mut refused = 0u32;
    for round in 0..10_000u64 {
        let op = round % 4;
        if call(op, round % 3) == EPERM {
            refused += 1;
        }
    }
    credentials::reset_for_task(me);
    check!(
        refused == 10_000,
        "only {refused} of 10000 calls were refused"
    );
    check!(
        watchdog::armed().is_none(),
        "a refused call armed the watchdog"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("power_arm_capability_gated", arm_is_capability_gated),
    ("power_arm_validates_stop", arm_validates_the_stop),
    ("power_watchdog_first_arm_wins", first_arm_wins),
    ("power_watchdog_fires_once", expiry_fires_once),
    ("power_watchdog_deadline_saturates", deadline_saturates),
    ("power_repeated_requests_safe", repeated_requests_are_safe),
    ("power_soak_arm_expire", soak_arm_expire_generations),
    ("power_soak_refused_calls", soak_refused_calls),
];
