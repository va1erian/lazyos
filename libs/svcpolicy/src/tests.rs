//! The supervision rules, one behaviour per test.

use super::*;

fn exit(policy: Restart, app: bool, status: u64, uptime: u64, restarts: u64) -> Exit {
    Exit {
        policy,
        app,
        status,
        uptime,
        restarts,
        resident: false,
    }
}

#[test]
fn a_crashing_service_backs_off_then_gives_up() {
    let mut restarts = 0;
    let mut delays = Vec::new();
    loop {
        match decide(exit(Restart::OnFailure, false, 1, 5, restarts)) {
            Outcome::Restart {
                restarts: next,
                delay,
            } => {
                restarts = next;
                delays.push(delay);
            }
            Outcome::Failed { restarts, cause } => {
                assert_eq!(cause, Cause::Exhausted);
                assert_eq!(restarts, MAX_RESTARTS);
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(delays, [10, 20, 40, 80]);
}

#[test]
fn a_stable_run_wipes_the_budget() {
    let outcome = decide(exit(Restart::OnFailure, false, 1, STABLE_TICKS, 4));
    assert_eq!(
        outcome,
        Outcome::Restart {
            restarts: 1,
            delay: BACKOFF_BASE
        }
    );
}

#[test]
fn backoff_is_capped() {
    assert_eq!(backoff(1), 10);
    assert_eq!(backoff(5), 160);
    assert_eq!(backoff(7), BACKOFF_MAX);
    assert_eq!(backoff(1000), BACKOFF_MAX);
}

#[test]
fn an_app_failing_at_start_up_is_not_restarted_and_tells_the_desktop() {
    let failed = exit(Restart::OnFailure, true, 2, 300, 0);
    let outcome = decide(failed);
    assert_eq!(
        outcome,
        Outcome::Failed {
            restarts: 0,
            cause: Cause::StartUp
        }
    );
    assert!(tells_desktop(&failed, &outcome));
}

#[test]
fn an_app_crashing_after_a_long_run_restarts_once_more() {
    let crashed = exit(Restart::OnFailure, true, 139, STARTUP_TICKS + 1, 3);
    let outcome = decide(crashed);
    assert_eq!(
        outcome,
        Outcome::Restart {
            restarts: 1,
            delay: BACKOFF_BASE
        }
    );
    assert!(!tells_desktop(&crashed, &outcome));
    // ...and when the restart fails starting, it stops there.
    let again = exit(Restart::OnFailure, true, 139, 20, 1);
    assert!(matches!(
        decide(again),
        Outcome::Failed {
            cause: Cause::StartUp,
            ..
        }
    ));
}

#[test]
fn a_service_failing_at_start_keeps_restarting() {
    let outcome = decide(exit(Restart::OnFailure, false, 2, 30, 0));
    assert!(matches!(outcome, Outcome::Restart { restarts: 1, .. }));
}

#[test]
fn the_desktop_shell_always_comes_back_and_never_notifies() {
    for status in [0, 2, 137] {
        let shell = exit(Restart::Always, true, status, 10, 0);
        let outcome = decide(shell);
        assert!(matches!(outcome, Outcome::Restart { .. }), "{status}");
        assert!(!tells_desktop(&shell, &outcome));
    }
}

#[test]
fn an_app_the_user_killed_is_stopped_quietly() {
    for status in [129, 130, 137, 143] {
        let killed = exit(Restart::OnFailure, true, status, 30, 0);
        let outcome = decide(killed);
        assert_eq!(outcome, Outcome::Stopped { restarts: 0 }, "{status}");
        assert!(!tells_desktop(&killed, &outcome));
    }
    // A service killed the same way is restarted, as before.
    assert!(matches!(
        decide(exit(Restart::OnFailure, false, 137, 30, 0)),
        Outcome::Restart { .. }
    ));
}

#[test]
fn a_clean_app_exit_is_a_stop() {
    let closed = exit(Restart::OnFailure, true, 0, 5, 0);
    let outcome = decide(closed);
    assert_eq!(outcome, Outcome::Stopped { restarts: 0 });
    assert!(!tells_desktop(&closed, &outcome));
}

#[test]
fn a_once_app_failing_is_failed_and_notifies() {
    let failed = exit(Restart::Once, true, 1, 5000, 0);
    let outcome = decide(failed);
    assert_eq!(
        outcome,
        Outcome::Failed {
            restarts: 0,
            cause: Cause::NoPolicy
        }
    );
    assert!(tells_desktop(&failed, &outcome));
}

#[test]
fn statuses_read_as_people_say_them() {
    assert_eq!(describe_status(2), "exit code 2");
    assert_eq!(describe_status(0), "exit code 0");
    assert_eq!(describe_status(139), "signal 11 (segmentation fault)");
    assert_eq!(describe_status(128 + 33), "signal 33");
    assert_eq!(describe_status(300), "exit code 300");
}

#[test]
fn reasons_are_one_bounded_line() {
    assert_eq!(
        clean_reason("  main_form.rhai:3:7:\n\tboom  \"x\"\r\n"),
        "main_form.rhai:3:7: boom \"x\""
    );
    assert_eq!(clean_reason("\u{1b}[31mred"), "[31mred");
    assert_eq!(clean_reason(""), "");
    let long = "é".repeat(MAX_REASON_BYTES);
    let kept = clean_reason(&long);
    assert!(kept.len() <= MAX_REASON_BYTES);
    assert_eq!(kept.len(), MAX_REASON_BYTES);
    assert!(kept.chars().all(|ch| ch == 'é'));
}

#[test]
fn every_policy_has_its_wire_word() {
    assert_eq!(Restart::Always.label(), "always");
    assert_eq!(Restart::OnFailure.label(), "on-failure");
    assert_eq!(Restart::Once.label(), "once");
}

#[test]
fn a_resident_app_restarts_after_a_crash_past_start_up_only() {
    let resident = |status, uptime| Exit {
        resident: true,
        ..exit(Restart::Once, true, status, uptime, 0)
    };
    // A crash after start-up: restarted with backoff, whatever the manifest.
    assert_eq!(
        decide(resident(139, STARTUP_TICKS + 1)),
        Outcome::Restart {
            restarts: 1,
            delay: BACKOFF_BASE
        }
    );
    // Failing while starting: the notice, no restart.
    assert!(matches!(
        decide(resident(1, 10)),
        Outcome::Failed {
            cause: Cause::StartUp,
            ..
        }
    ));
    // A clean exit (it chose to) and a kill by the user: stopped.
    assert!(matches!(decide(resident(0, STARTUP_TICKS + 1)), Outcome::Stopped { .. }));
    assert!(matches!(decide(resident(137, STARTUP_TICKS + 1)), Outcome::Stopped { .. }));
    // Every such run lasted past start-up, so it counts as recovered: the
    // restart count resets and the backoff stays at its first step.
    let again = Exit {
        restarts: 3,
        ..resident(139, STARTUP_TICKS + 1)
    };
    assert!(matches!(decide(again), Outcome::Restart { restarts: 1, .. }));
    // `resident` only means something for an app.
    let service = Exit {
        resident: true,
        ..exit(Restart::Once, false, 1, STARTUP_TICKS + 1, 0)
    };
    assert!(matches!(decide(service), Outcome::Failed { cause: Cause::NoPolicy, .. }));
}
