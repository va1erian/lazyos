//! Lookup tables, all fixed point.

/// Amiga PAL Paula clock in Hz.
pub const PAL_CLOCK: u64 = 3_546_895;
/// Shortest and longest period the three-octave range spans.
pub const MIN_PERIOD: u16 = 113;
pub const MAX_PERIOD: u16 = 856;

/// `2^(-ft/96)` in Q16 for finetune `-8..=7` (index `ft + 8`): scales a
/// period so a positive finetune raises the pitch by `ft/8` semitone.
const FINETUNE: [u32; 16] = [
    69433, 68933, 68438, 67945, 67456, 66971, 66489, 66011, 65536, 65065, 64596, 64132, 63670,
    63212, 62757, 62306,
];

/// `2^(-n/12)` in Q16: the period ratio of `n` semitones up.
const SEMITONE: [u32; 16] = [
    65536, 61858, 58386, 55109, 52016, 49097, 46341, 43740, 41285, 38968, 36781, 34716, 32768,
    30929, 29193, 27554,
];

/// ProTracker's vibrato sine, `255 * sin(i * pi / 32)`.
pub const SINE: [u8; 32] = [
    0, 25, 50, 74, 98, 120, 142, 162, 180, 197, 212, 225, 236, 244, 250, 254, 255, 254, 250, 244,
    236, 225, 212, 197, 180, 162, 142, 120, 98, 74, 50, 25,
];

fn scale(period: u16, q16: u32) -> u16 {
    ((u64::from(period) * u64::from(q16) + 0x8000) >> 16).min(u64::from(u16::MAX)) as u16
}

/// A note's period adjusted for a sample's finetune (`-8..=7`).
pub fn finetuned(period: u16, finetune: i8) -> u16 {
    let index = (i32::from(finetune) + 8).clamp(0, 15) as usize;
    scale(period, FINETUNE[index]).clamp(MIN_PERIOD, MAX_PERIOD)
}

/// `period` raised by `semitones` (`0..=15`, larger values clamp).
pub fn semitones_up(period: u16, semitones: u8) -> u16 {
    scale(period, SEMITONE[usize::from(semitones.min(15))]).max(1)
}
