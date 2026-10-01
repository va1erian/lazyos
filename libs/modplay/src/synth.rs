//! Build small `.mod` files in memory: the fixtures for the unit tests, the
//! fuzz seeds and the demo module. Host-only (`cfg(test)` or the `fuzz`
//! feature).

use std::vec;
use std::vec::Vec;

use crate::module::{Note, CHANNELS, ROWS_PER_PATTERN};

const HEADER: usize = 1084;

/// One sample slot of the file being built.
#[derive(Clone, Default)]
struct Slot {
    data: Vec<i8>,
    volume: u8,
    finetune: u8,
    loop_start: usize,
    loop_len: usize,
}

/// A `.mod` under construction. `build` writes a valid `M.K.` file.
pub struct ModBuilder {
    slots: Vec<Slot>,
    orders: Vec<u8>,
    patterns: Vec<Vec<Note>>,
}

impl Default for ModBuilder {
    fn default() -> ModBuilder {
        ModBuilder::new()
    }
}

impl ModBuilder {
    /// One empty pattern, an order list of `[0]` and no samples.
    pub fn new() -> ModBuilder {
        ModBuilder {
            slots: vec![Slot::default(); 31],
            orders: vec![0],
            patterns: vec![vec![Note::default(); ROWS_PER_PATTERN * CHANNELS]],
        }
    }

    /// Sample `number` (`1..=31`): `data`, default `volume`, looping over the
    /// whole sample when `looped`.
    pub fn sample(mut self, number: usize, data: Vec<i8>, volume: u8, looped: bool) -> Self {
        let slot = &mut self.slots[number - 1];
        slot.loop_start = 0;
        slot.loop_len = if looped { data.len() } else { 0 };
        slot.data = data;
        slot.volume = volume;
        self
    }

    /// Finetune nibble (`0..=15`, as stored) of sample `number`.
    pub fn finetune(mut self, number: usize, nibble: u8) -> Self {
        self.slots[number - 1].finetune = nibble & 15;
        self
    }

    /// Replace the order list.
    pub fn orders(mut self, orders: &[u8]) -> Self {
        self.orders = orders.to_vec();
        let wanted = orders
            .iter()
            .copied()
            .max()
            .map_or(1, |m| usize::from(m) + 1);
        while self.patterns.len() < wanted {
            self.patterns
                .push(vec![Note::default(); ROWS_PER_PATTERN * CHANNELS]);
        }
        self
    }

    /// Put `note` in `pattern` at `(row, channel)`.
    pub fn note(mut self, pattern: usize, row: usize, channel: usize, note: Note) -> Self {
        self.patterns[pattern][row * CHANNELS + channel] = note;
        self
    }

    /// The file's bytes.
    pub fn build(&self) -> Vec<u8> {
        let mut out = vec![0u8; HEADER];
        out[..5].copy_from_slice(b"synth");
        for (i, slot) in self.slots.iter().enumerate() {
            let at = 20 + i * 30 + 22;
            out[at..at + 2].copy_from_slice(&((slot.data.len() / 2) as u16).to_be_bytes());
            out[at + 2] = slot.finetune;
            out[at + 3] = slot.volume;
            out[at + 4..at + 6].copy_from_slice(&((slot.loop_start / 2) as u16).to_be_bytes());
            let loop_words = if slot.loop_len > 0 {
                slot.loop_len / 2
            } else {
                1
            };
            out[at + 6..at + 8].copy_from_slice(&(loop_words as u16).to_be_bytes());
        }
        out[950] = self.orders.len() as u8;
        out[952..952 + self.orders.len()].copy_from_slice(&self.orders);
        out[1080..1084].copy_from_slice(b"M.K.");
        for pattern in &self.patterns {
            for n in pattern {
                out.push((n.sample & 0xF0) | ((n.period >> 8) as u8 & 0x0F));
                out.push(n.period as u8);
                out.push(((n.sample & 0x0F) << 4) | (n.effect & 0x0F));
                out.push(n.param);
            }
        }
        for slot in &self.slots {
            out.extend(slot.data.iter().map(|&b| b as u8));
            // Sample lengths are in words; pad an odd byte count.
            if slot.data.len() % 2 == 1 {
                out.push(0);
            }
        }
        out
    }
}

/// A cell with `sample` at `period`.
pub fn note(period: u16, sample: u8) -> Note {
    Note {
        period,
        sample,
        effect: 0,
        param: 0,
    }
}

/// A cell with only an effect.
pub fn effect(effect: u8, param: u8) -> Note {
    Note {
        period: 0,
        sample: 0,
        effect,
        param,
    }
}

/// One cycle of a square wave, `len` bytes (even), full scale.
pub fn square(len: usize) -> Vec<i8> {
    (0..len)
        .map(|i| if i < len / 2 { 127 } else { -128 })
        .collect()
}
