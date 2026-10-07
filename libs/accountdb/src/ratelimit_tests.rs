//! The brake's three rules (review of #659, H5): who counts where, a success
//! clears names only, and a locked slot is never evicted.

use alloc::format;

use crate::ratelimit::{Attempt, Key, Limiter, FORGET_AFTER, FREE_FAILURES, MAX_SLOTS};

/// The login screen (`logind`) or the trusted prompt (`elevd`).
const MEDIATOR: u32 = 905;
/// The session user.
const SESSION: u32 = 1000;

/// One failed attempt, as `accountsd` records it: refused when the brake
/// holds, counted otherwise. Returns whether it was checked.
fn fail(limiter: &mut Limiter, attempt: &Attempt, now: u64) -> bool {
    if limiter.check(&attempt.checked, now).is_err() {
        return false;
    }
    limiter.failed(&attempt.counted, now);
    true
}

#[test]
fn a_session_flooding_admin_never_locks_admin_for_the_mediators() {
    let mut limiter = Limiter::new();
    let flood = Attempt::new("admin", SESSION, false);
    assert_eq!(flood.counted, [Key::Caller(SESSION)]);
    let mut checked = 0;
    // Ten minutes of guesses, one per tenth of a second.
    for now in (0..60_000).step_by(10) {
        checked += u32::from(fail(&mut limiter, &flood, now));
    }
    // The session slowed itself...
    assert!(checked < 30, "{checked} guesses were checked");
    assert!(limiter.check(&flood.checked, 60_000).is_err());
    // ...and logind/elevd still check admin's password at once.
    let login = Attempt::new("admin", MEDIATOR, true);
    assert_eq!(login.checked, [Key::Name("admin".into())]);
    assert_eq!(limiter.check(&login.checked, 60_000), Ok(()));
    assert_eq!(limiter.failures(&Key::Name("admin".into()), 60_000), 0);
}

#[test]
fn a_mediator_s_failures_lock_the_name_for_everyone() {
    let mut limiter = Limiter::new();
    let login = Attempt::new("admin", MEDIATOR, true);
    for now in 0..=u64::from(FREE_FAILURES) {
        assert!(fail(&mut limiter, &login, now));
    }
    assert!(limiter.check(&login.checked, 5).is_err());
    // A session trying the same name is held by it too.
    let session = Attempt::new("admin", SESSION, false);
    assert!(limiter.check(&session.checked, 5).is_err());
}

#[test]
fn a_success_never_resets_the_caller_s_lock() {
    let mut limiter = Limiter::new();
    let guess = Attempt::new("admin", SESSION, false);
    for now in 0..6 {
        limiter.failed(&guess.counted, now);
    }
    // The session then types its own password right.
    let own = Attempt::new("user", SESSION, false);
    assert_eq!(own.cleared, Some(Key::Name("user".into())));
    limiter.succeeded(core::slice::from_ref(own.cleared.as_ref().unwrap()));
    assert!(limiter.check(&guess.checked, 7).is_err());
    assert_eq!(limiter.failures(&Key::Caller(SESSION), 7), 6);
    // Even when handed every key, a success clears the names alone.
    limiter.succeeded(&[Key::Caller(SESSION), Key::Name("admin".into())]);
    assert_eq!(limiter.failures(&Key::Caller(SESSION), 7), 6);
}

#[test]
fn a_success_clears_the_authenticated_name() {
    let mut limiter = Limiter::new();
    let login = Attempt::new("user", MEDIATOR, true);
    for now in 0..2 {
        limiter.failed(&login.counted, now);
    }
    limiter.succeeded(core::slice::from_ref(login.cleared.as_ref().unwrap()));
    assert_eq!(limiter.failures(&Key::Name("user".into()), 3), 0);
}

#[test]
fn a_full_table_never_drops_the_admin_lock() {
    let mut limiter = Limiter::new();
    let admin = Key::Name("admin".into());
    for now in 0..6 {
        limiter.failed(core::slice::from_ref(&admin), now);
    }
    // Flood with many more keys than the table holds, each one penalized.
    for n in 0..(3 * MAX_SLOTS as u32) {
        let key = [Key::Name(format!("n{n}"))];
        for step in 0u32..6 {
            if limiter.check(&key, 10 + u64::from(step)).is_ok() {
                limiter.failed(&key, 10 + u64::from(step));
            }
        }
    }
    assert!(limiter.len() <= MAX_SLOTS);
    assert!(limiter.check(core::slice::from_ref(&admin), 20).is_err());
    assert_eq!(limiter.failures(&admin, 20), 6);
    // A new key the full table cannot hold is refused, not let through.
    assert!(limiter.check(&[Key::Caller(4242)], 20).is_err());
    // Once the old slots are forgotten, there is room again.
    let later = 20 + FORGET_AFTER;
    assert_eq!(limiter.check(&[Key::Caller(4242)], later), Ok(()));
}

#[test]
fn an_invalid_name_is_never_a_key() {
    let attempt = Attempt::new("../etc", SESSION, false);
    assert_eq!(attempt.checked, [Key::Caller(SESSION)]);
    assert_eq!(attempt.cleared, None);
    let attempt = Attempt::new("Nobody", MEDIATOR, true);
    assert!(attempt.checked.is_empty() && attempt.counted.is_empty());
}
