//! The timer queue on its own: a local `TimerQueue`, no task table.

use alloc::boxed::Box;

use super::*;
use crate::task::timerq::TimerQueue;
use crate::task::MAX_TASKS;

fn fresh_queue() -> Box<TimerQueue> {
    Box::new(TimerQueue::new())
}

/// Pop everything, checking the order: deadlines never go back, and equal
/// deadlines come out by slot.
fn drain_in_order(queue: &mut TimerQueue) -> Result<usize, String> {
    let mut last: Option<(u64, usize)> = None;
    let mut count = 0;
    while let Some(entry) = queue.pop_due(u64::MAX) {
        check!(
            last.is_none_or(|prev| prev < entry),
            "popped {entry:?} after {last:?}"
        );
        queue.check().map_err(String::from)?;
        last = Some(entry);
        count += 1;
    }
    Ok(count)
}

/// Random deadlines on every slot come out sorted, with the heap and index
/// consistent after every step.
pub fn order() -> Result<(), String> {
    let mut queue = fresh_queue();
    let mut rng = Rng(0x5eed_0001);
    for slot in 0..MAX_TASKS {
        queue.arm(slot, rng.below(1_000_000));
        queue.check().map_err(String::from)?;
    }
    check!(queue.len() == MAX_TASKS, "{} queued", queue.len());
    let earliest = queue.peek().ok_or("empty after arming")?;
    let drained = drain_in_order(&mut queue)?;
    check!(drained == MAX_TASKS, "drained {drained}");
    check!(queue.is_empty(), "not empty after draining");
    check!(earliest.0 < 1_000_000, "peek {earliest:?}");
    // Out-of-range slots are ignored.
    queue.arm(MAX_TASKS, 5);
    check!(queue.is_empty(), "an out-of-range slot was queued");
    Ok(())
}

/// Cancelling removes exactly one slot's entry, a second cancel is a no-op,
/// and re-arming a queued slot moves it (earlier and later).
pub fn cancel() -> Result<(), String> {
    let mut queue = fresh_queue();
    for slot in 0..100 {
        queue.arm(slot, 1_000 + slot as u64 * 10);
    }
    for slot in (0..100).step_by(3) {
        check!(queue.cancel(slot), "cancel {slot} found nothing");
        check!(!queue.cancel(slot), "second cancel {slot} found something");
        queue.check().map_err(String::from)?;
    }
    // Move slot 50 to the front and slot 1 to the back.
    queue.arm(50, 1);
    queue.arm(1, 1_000_000);
    queue.check().map_err(String::from)?;
    check!(queue.peek() == Some((1, 50)), "peek {:?}", queue.peek());
    check!(
        queue.deadline_of(1) == Some(1_000_000),
        "slot 1 {:?}",
        queue.deadline_of(1)
    );
    check!(queue.deadline_of(3).is_none(), "cancelled slot 3 is queued");
    let mut seen = [false; 100];
    while let Some((_, slot)) = queue.pop_due(u64::MAX) {
        check!(slot % 3 != 0, "cancelled slot {slot} expired");
        seen[slot] = true;
    }
    let missing = (0..100).filter(|&s| s % 3 != 0 && !seen[s]).count();
    check!(missing == 0, "{missing} armed slots never expired");
    Ok(())
}

/// Many timers at one deadline: none expires a nanosecond early, all expire
/// at it, in slot order.
pub fn same_deadline() -> Result<(), String> {
    let mut queue = fresh_queue();
    const AT: u64 = 7_777_777;
    for slot in (0..200).rev() {
        queue.arm(slot, AT);
    }
    check!(
        queue.pop_due(AT - 1).is_none(),
        "an entry expired before its deadline"
    );
    for expected in 0..200 {
        let popped = queue.pop_due(AT).ok_or("an entry did not expire")?;
        check!(
            popped == (AT, expected),
            "popped {popped:?}, expected slot {expected}"
        );
    }
    check!(queue.is_empty(), "left {} entries", queue.len());
    Ok(())
}

/// A deadline in the past is due at once; the far end of `u64` orders and
/// saturates; tick conversion and the APIC count stay in range.
pub fn past_and_wrap() -> Result<(), String> {
    let mut queue = fresh_queue();
    queue.arm(3, 0);
    check!(queue.pop_due(0) == Some((0, 3)), "deadline 0 not due at 0");
    queue.arm(1, u64::MAX);
    queue.arm(2, u64::MAX - 1);
    queue.arm(4, 1);
    check!(queue.pop_due(0).is_none(), "deadline 1 due at 0");
    check!(queue.pop_due(u64::MAX - 2) == Some((1, 4)), "deadline 1");
    check!(
        queue.pop_due(u64::MAX - 2).is_none(),
        "u64::MAX - 1 due early"
    );
    check!(
        queue.pop_due(u64::MAX) == Some((u64::MAX - 1, 2)),
        "MAX - 1"
    );
    check!(queue.pop_due(u64::MAX) == Some((u64::MAX, 1)), "MAX");
    check!(task::ticks_to_ns(5) == 50_000_000, "5 ticks");
    check!(task::ticks_to_ns(u64::MAX) == u64::MAX, "tick saturation");
    check!(
        task::ticks_to_ns(u64::MAX >> 2) == u64::MAX,
        "the power watchdog's clamp saturates"
    );
    use crate::arch::event_timer::counts_for;
    check!(counts_for(0, 62_500_000) == 1, "zero interval");
    check!(
        counts_for(1_000_000, 62_500_000) == 62_500,
        "1 ms at 62.5 MHz"
    );
    check!(counts_for(u64::MAX, u64::MAX) == u32::MAX, "clamped");
    Ok(())
}

/// A million seeded arm/cancel/pop operations against a model array: the
/// queue's earliest entry always matches the model's, and the invariants
/// hold throughout.
pub fn soak() -> Result<(), String> {
    const OPS: u32 = 1_000_000;
    let mut queue = fresh_queue();
    let mut model: Box<[Option<u64>; MAX_TASKS]> = Box::new([None; MAX_TASKS]);
    let mut rng = Rng(0x5eed_0002);
    let mut now = 0u64;
    let (mut armed, mut cancelled, mut expired) = (0u64, 0u64, 0u64);
    for op in 0..OPS {
        let slot = rng.below(MAX_TASKS as u64) as usize;
        match rng.below(8) {
            0..=3 => {
                let deadline = now + rng.below(10_000);
                queue.arm(slot, deadline);
                model[slot] = Some(deadline);
                armed += 1;
            }
            4 | 5 => {
                let had = queue.cancel(slot);
                check!(had == model[slot].is_some(), "cancel {slot} at op {op}");
                model[slot] = None;
                cancelled += u64::from(had);
            }
            _ => {
                now += rng.below(200);
                while let Some((deadline, slot)) = queue.pop_due(now) {
                    check!(
                        model[slot] == Some(deadline) && deadline <= now,
                        "popped ({deadline}, {slot}) at {now}, model {:?}",
                        model[slot]
                    );
                    model[slot] = None;
                    expired += 1;
                }
            }
        }
        if op % 1024 == 0 {
            queue.check().map_err(|e| format!("op {op}: {e}"))?;
            let want = model
                .iter()
                .enumerate()
                .filter_map(|(slot, d)| d.map(|d| (d, slot)))
                .min();
            check!(
                queue.peek() == want,
                "op {op}: peek {:?} != {want:?}",
                queue.peek()
            );
        }
    }
    serial_println!(
        "TEST:deadline_queue_soak:INFO:{OPS} ops: {armed} armed, {cancelled} cancelled, {expired} expired"
    );
    let left = drain_in_order(&mut queue)?;
    let modelled = model.iter().filter(|d| d.is_some()).count();
    check!(left == modelled, "{left} left, model has {modelled}");
    Ok(())
}
