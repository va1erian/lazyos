//! Nice values onto the stride scheduler's per-task weight.
//!
//! Linux weighs a task `1024 / 1.25^nice` (`sched_prio_to_weight`), so one
//! nice step is a quarter more or less CPU. LazyOS shares a class by an
//! integer weight of [`MIN_WEIGHT`]..=[`MAX_WEIGHT`] (1..=32) and a Normal task
//! weighs 2, so the same ratios are applied to the class's default weight and
//! rounded: nice 0 is the default, the positive steps run out of room at 1
//! quickly (nice 2 and up weigh 1), and the negative ones grow to the cap
//! (nice -20 weighs 32). It is a coarse mapping because the scale is; the
//! order is always kept (a lower nice never weighs less).

use super::sched::{MAX_WEIGHT, MIN_WEIGHT};

/// Linux's `sched_prio_to_weight`, nice -20 first.
const PRIO_TO_WEIGHT: [u32; 40] = [
    88761, 71755, 56483, 46273, 36291, 29154, 23254, 18705, 14949, 11916, 9548, 7620, 6100, 4904,
    3906, 3121, 2501, 1991, 1586, 1277, 1024, 820, 655, 526, 423, 335, 272, 215, 172, 137, 110, 87,
    70, 56, 45, 36, 29, 23, 18, 15,
];

/// Smallest and largest nice value.
pub const MIN_NICE: i32 = -20;
pub const MAX_NICE: i32 = 19;

/// The weight of a task of class-default weight `base` at `nice`; a
/// `SCHED_IDLE` task always weighs the minimum.
pub fn weight(base: u16, nice: i32, idle: bool) -> u16 {
    if idle {
        return MIN_WEIGHT;
    }
    let nice = nice.clamp(MIN_NICE, MAX_NICE);
    let scaled = u32::from(base) * PRIO_TO_WEIGHT[(nice - MIN_NICE) as usize];
    let rounded = (scaled + 512) / 1024;
    rounded.clamp(u32::from(MIN_WEIGHT), u32::from(MAX_WEIGHT)) as u16
}
