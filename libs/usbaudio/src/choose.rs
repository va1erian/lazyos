//! Which alternate setting and rate a stream runs at.
//!
//! The mixer always asks for stereo 16-bit at 48 kHz; the device may offer
//! any mix of channel counts, sample sizes and rates. [`choose`] keeps the
//! rate when any alternate has it (the mixer then needs no resampling),
//! else takes the supported standard rate closest to it; among the
//! alternates at that rate it prefers two channels, then the smallest
//! subslot of at least 16 bits, then the smallest packet (least bus time,
//! and the fallback when the bus has no room for a larger one).

use alloc::vec::Vec;

use crate::desc::{Alt, Function};
use crate::STANDARD_RATES;

/// One alternate (by index into `Function::alts`) at one rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choice {
    pub alt: usize,
    pub rate: u32,
}

/// The [`STANDARD_RATES`] bitmap of what `alt` supports.
pub fn rate_mask(alt: &Alt) -> u32 {
    STANDARD_RATES
        .iter()
        .enumerate()
        .filter(|(_, &rate)| alt.rates.contains(rate))
        .fold(0, |mask, (bit, _)| mask | 1 << bit)
}

/// The bitmap of every rate some alternate of `function` supports.
pub fn function_rates(function: &Function) -> u32 {
    function.alts.iter().map(rate_mask).fold(0, |a, b| a | b)
}

/// How well an alternate fits once the rate is settled: lower is better.
fn rank(alt: &Alt) -> (u8, u8, u32) {
    let channels = if alt.format.channels == 2 { 0 } else { 1 };
    let subslot = if alt.format.bits >= 16 {
        alt.format.subslot
    } else {
        // An 8-bit stream loses half the mixer's resolution: last resort.
        u8::MAX
    };
    (channels, subslot, alt.data.max_esit())
}

/// The rate a stream asked for `wanted` should run at: `wanted` when some
/// alternate has it, else the closest standard rate any alternate has,
/// else (a device with only odd rates) the closest rate any range starts
/// or ends at.
pub fn settle_rate(function: &Function, wanted: u32) -> Option<u32> {
    let supported = |rate: u32| function.alts.iter().any(|alt| alt.rates.contains(rate));
    if supported(wanted) {
        return Some(wanted);
    }
    let distance = |rate: &u32| rate.abs_diff(wanted);
    if let Some(rate) = STANDARD_RATES
        .iter()
        .copied()
        .filter(|&rate| supported(rate))
        .min_by_key(distance)
    {
        return Some(rate);
    }
    function
        .alts
        .iter()
        .flat_map(|alt| alt.rates.ranges.iter())
        .flat_map(|range| [range.min, range.max])
        .min_by_key(distance)
}

/// The best alternate for `wanted` Hz, skipping the alternates in
/// `refused` (the bus had no bandwidth for them).
pub fn choose(function: &Function, wanted: u32, refused: &[usize]) -> Option<Choice> {
    let rate = settle_rate(function, wanted)?;
    let mut candidates: Vec<usize> = (0..function.alts.len())
        .filter(|index| !refused.contains(index))
        .filter(|&index| function.alts[index].rates.contains(rate))
        .collect();
    candidates.sort_by_key(|&index| rank(&function.alts[index]));
    let alt = *candidates.first()?;
    Some(Choice { alt, rate })
}

/// The alternate to fall back to after `failed` was refused for bandwidth:
/// the best one at the same rate whose packets are smaller.
pub fn smaller(function: &Function, failed: usize, rate: u32, refused: &[usize]) -> Option<usize> {
    let limit = function.alts.get(failed)?.data.max_esit();
    let mut candidates: Vec<usize> = (0..function.alts.len())
        .filter(|index| !refused.contains(index) && *index != failed)
        .filter(|&index| {
            let alt = &function.alts[index];
            alt.rates.contains(rate) && alt.data.max_esit() < limit
        })
        .collect();
    candidates.sort_by_key(|&index| rank(&function.alts[index]));
    candidates.first().copied()
}
