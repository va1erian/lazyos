//! The focus barrier: keys typed right after a click wait for the focus the
//! click gives, keep their order, and are never lost to a silent compositor.

use alloc::string::String;
use alloc::vec::Vec;

use crate::barrier::{Barrier, HOLD_TICKS, MAX_HELD};
use crate::Output;

fn text(s: &str) -> Output {
    Output::Text(String::from(s))
}

fn texts(outputs: &[Output]) -> Vec<String> {
    outputs
        .iter()
        .map(|output| match output {
            Output::Text(text) => text.clone(),
            other => panic!("unexpected {other:?}"),
        })
        .collect()
}

#[test]
fn nothing_is_held_without_a_press() {
    let mut barrier = Barrier::new();
    let mut outputs = alloc::vec![text("a")];
    assert!(barrier.hold(&mut outputs).is_empty());
    assert_eq!(texts(&outputs), ["a"], "left to be delivered at once");
    assert!(!barrier.holding());
    assert_eq!(barrier.next_due(), None);
    assert!(barrier.settled(u64::MAX).is_empty());
}

#[test]
fn keys_after_a_press_wait_for_the_compositor_and_keep_their_order() {
    let mut barrier = Barrier::new();
    barrier.pressed(10, 100);
    assert!(barrier.holding());
    for key in ["e", "c", "h", "o"] {
        let mut outputs = alloc::vec![text(key)];
        assert!(barrier.hold(&mut outputs).is_empty());
        assert!(outputs.is_empty(), "taken into the hold");
    }
    // A note for an older pointer event does not cover the press.
    assert!(barrier.settled(9).is_empty());
    assert!(barrier.holding());
    assert_eq!(texts(&barrier.settled(10)), ["e", "c", "h", "o"]);
    assert!(!barrier.holding());
    let mut after = alloc::vec![text("!")];
    assert!(barrier.hold(&mut after).is_empty());
    assert_eq!(texts(&after), ["!"]);
}

#[test]
fn a_silent_compositor_costs_a_delay_not_the_keys() {
    let mut barrier = Barrier::new();
    barrier.pressed(3, 50);
    assert_eq!(barrier.next_due(), Some(50 + HOLD_TICKS));
    let mut outputs = alloc::vec![text("x")];
    barrier.hold(&mut outputs);
    assert!(barrier.expire(50 + HOLD_TICKS - 1).is_empty());
    assert_eq!(texts(&barrier.expire(50 + HOLD_TICKS)), ["x"]);
    assert!(!barrier.holding());
    assert_eq!(barrier.next_due(), None);
}

#[test]
fn a_second_press_extends_the_hold_to_itself() {
    let mut barrier = Barrier::new();
    barrier.pressed(5, 0);
    barrier.pressed(8, 10);
    assert_eq!(barrier.next_due(), Some(10 + HOLD_TICKS));
    let mut outputs = alloc::vec![text("k")];
    barrier.hold(&mut outputs);
    assert!(
        barrier.settled(5).is_empty(),
        "the first press alone is not enough"
    );
    assert_eq!(texts(&barrier.settled(8)), ["k"]);
    // An out-of-order (older) press never moves the target back.
    barrier.pressed(20, 0);
    barrier.pressed(15, 0);
    assert!(barrier.settled(15).is_empty());
    assert!(barrier.settled(20).is_empty(), "nothing was held");
    assert!(!barrier.holding());
}

#[test]
fn the_hold_is_bounded() {
    let mut barrier = Barrier::new();
    barrier.pressed(1, 0);
    let mut delivered = Vec::new();
    for n in 0..MAX_HELD + 10 {
        let mut outputs = alloc::vec![text(&alloc::format!("{n}"))];
        let released = barrier.hold(&mut outputs);
        if n + 1 < MAX_HELD {
            assert!(
                released.is_empty() && barrier.holding(),
                "still held at {n}"
            );
        }
        delivered.extend(released);
        delivered.extend(outputs);
    }
    assert!(!barrier.holding(), "the bound gave the hold up");
    let order: Vec<String> = (0..MAX_HELD + 10).map(|n| alloc::format!("{n}")).collect();
    assert_eq!(texts(&delivered), order, "nothing lost or reordered");
}

#[test]
fn soak_many_clicks_lose_and_reorder_nothing() {
    let mut barrier = Barrier::new();
    let mut delivered = Vec::new();
    let mut seq = 0u64;
    let mut now = 0u64;
    for round in 0..10_000u32 {
        seq += 1 + u64::from(round % 3);
        now += 1;
        barrier.pressed(seq, now);
        for key in 0..(round % 5) {
            let mut outputs = alloc::vec![text(&alloc::format!("{round}.{key}"))];
            delivered.extend(barrier.hold(&mut outputs));
            delivered.extend(outputs);
        }
        // Answered, late, or never (the timeout).
        match round % 4 {
            0 | 1 => delivered.extend(barrier.settled(seq)),
            2 => delivered.extend(barrier.settled(seq - 1)),
            _ => {
                now += HOLD_TICKS;
                delivered.extend(barrier.expire(now));
            }
        }
    }
    delivered.extend(barrier.release());
    let expected: Vec<String> = (0..10_000u32)
        .flat_map(|round| (0..round % 5).map(move |key| alloc::format!("{round}.{key}")))
        .collect();
    assert_eq!(texts(&delivered), expected);
}
