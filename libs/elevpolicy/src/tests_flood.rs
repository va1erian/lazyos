//! The prompt-flood brake, the request queue, the session records and the
//! unlabelled callers' approvals (review of #659, H4).

use crate::approvals::{Approvals, Caller};
use crate::backoff::{
    hold_ticks, Backoff, Hold, FIRST_HOLD_TICKS, FORGET_TICKS, MAX_HOLD_TICKS, MAX_TRACKED,
    QUIET_TICKS,
};
use crate::queue::{Queue, MAX_WAITING};
use crate::sessions::Sessions;
use crate::Class;

const APP: Caller = Caller {
    uid: 1000,
    label: 7,
    session: 1,
};

#[test]
fn holds_double_up_to_the_cap() {
    assert_eq!(hold_ticks(1), FIRST_HOLD_TICKS);
    assert_eq!(hold_ticks(2), 2 * FIRST_HOLD_TICKS);
    assert_eq!(hold_ticks(3), 4 * FIRST_HOLD_TICKS);
    assert_eq!(hold_ticks(40), MAX_HOLD_TICKS);
    assert_eq!(hold_ticks(u32::MAX), MAX_HOLD_TICKS);
}

#[test]
fn a_cancel_holds_the_caller_and_pauses_everyone() {
    let mut backoff = Backoff::new();
    assert_eq!(backoff.check(APP, 0), Ok(()));
    backoff.unanswered(APP, 100);
    // The asker waits the first hold, without a prompt.
    assert_eq!(
        backoff.check(APP, 101),
        Err(Hold::Caller {
            until: 100 + FIRST_HOLD_TICKS
        })
    );
    // Everyone else waits the quiet pause, then may ask.
    let other = Caller { label: 8, ..APP };
    assert_eq!(
        backoff.check(other, 101),
        Err(Hold::Quiet {
            until: 100 + QUIET_TICKS
        })
    );
    assert_eq!(backoff.check(other, 100 + QUIET_TICKS), Ok(()));
    assert_eq!(backoff.check(APP, 100 + FIRST_HOLD_TICKS), Ok(()));
}

#[test]
fn repeated_cancels_grow_the_hold_and_an_approval_ends_it() {
    let mut backoff = Backoff::new();
    let mut now = 0;
    for count in 1..=10u32 {
        backoff.unanswered(APP, now);
        let until = backoff.check(APP, now).unwrap_err().until();
        assert_eq!(until - now, hold_ticks(count), "hold {count}");
        now = until;
    }
    assert_eq!(backoff.check(APP, now), Ok(()));
    backoff.unanswered(APP, now);
    assert_eq!(
        backoff.check(APP, now + 1).unwrap_err().until() - now,
        MAX_HOLD_TICKS
    );
    backoff.approved(APP);
    assert_eq!(backoff.check(APP, now + QUIET_TICKS), Ok(()));
    // The count started again.
    backoff.unanswered(APP, now + QUIET_TICKS);
    assert_eq!(
        backoff.check(APP, now + QUIET_TICKS).unwrap_err().until(),
        now + QUIET_TICKS + FIRST_HOLD_TICKS
    );
}

#[test]
fn a_quiet_spell_forgets_the_count() {
    let mut backoff = Backoff::new();
    backoff.unanswered(APP, 0);
    backoff.unanswered(APP, FIRST_HOLD_TICKS);
    let later = FIRST_HOLD_TICKS + hold_ticks(2) + FORGET_TICKS;
    backoff.unanswered(APP, later);
    assert_eq!(
        backoff.check(APP, later).unwrap_err().until(),
        later + FIRST_HOLD_TICKS
    );
}

#[test]
fn holds_are_per_caller_and_end_with_the_session() {
    let mut backoff = Backoff::new();
    backoff.unanswered(APP, 0);
    let elsewhere = Caller { session: 2, ..APP };
    assert_eq!(backoff.check(elsewhere, QUIET_TICKS), Ok(()));
    backoff.end_session(1);
    assert_eq!(backoff.check(APP, QUIET_TICKS), Ok(()));
}

#[test]
fn the_hold_table_is_bounded_and_keeps_live_holds() {
    let mut backoff = Backoff::new();
    backoff.unanswered(APP, 1_000_000);
    for uid in 0..(MAX_TRACKED as u32 * 3) {
        backoff.unanswered(
            Caller {
                uid,
                label: 1,
                session: 9,
            },
            uid as u64,
        );
    }
    // The flood of old holds pushed out old ones, not the newest.
    assert!(matches!(
        backoff.check(APP, 1_000_001),
        Err(Hold::Caller { .. })
    ));
}

#[test]
fn one_request_per_caller_and_a_bounded_queue() {
    let mut queue = Queue::new();
    // The caller being answered may not queue another.
    assert_eq!(queue.admit(Some(APP), APP, 1), Err(1));
    let other = Caller { label: 8, ..APP };
    assert_eq!(queue.admit(Some(APP), other, 2), Ok(()));
    assert_eq!(queue.admit(Some(APP), other, 3), Err(3));
    for label in 100..(100 + MAX_WAITING as u32 - 1) {
        assert_eq!(queue.admit(None, Caller { label, ..APP }, 4), Ok(()));
    }
    assert_eq!(queue.len(), MAX_WAITING);
    assert_eq!(queue.admit(None, Caller { label: 99, ..APP }, 5), Err(5));
    assert_eq!(queue.pop(), Some((other, 2)));
    // Once out of the queue (being answered), only `active` holds it back.
    assert_eq!(queue.admit(None, other, 6), Ok(()));
}

#[test]
fn session_records_end_old_ids() {
    let mut sessions = Sessions::new();
    assert!(sessions.record(3, "starting", 0));
    assert!(!sessions.record(3, "active", 40));
    assert!(!sessions.record(3, "active", 40));
    // `logind` restarted and reused the id for another shell.
    assert!(sessions.record(3, "active", 41));
    assert!(sessions.record(3, "exited", 41));
    assert!(!sessions.record(3, "active", 50));
}

#[test]
fn unlabelled_callers_get_no_standing_view() {
    let mut approvals = Approvals::new();
    let shell = Caller { label: 0, ..APP };
    approvals.grant(shell, Class::View, 0);
    assert!(!approvals.covers(shell, Class::View, 1));
    approvals.grant(APP, Class::View, 0);
    assert!(approvals.covers(APP, Class::View, 1));
}
