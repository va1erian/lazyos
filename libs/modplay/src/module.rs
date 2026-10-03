//! The parsed, validated module: header, samples and pattern data.

use alloc::vec::Vec;

/// Channels in a supported module (classic Amiga four).
pub const CHANNELS: usize = 4;
/// Rows in every pattern.
pub const ROWS_PER_PATTERN: usize = 64;

/// Why a file was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModError {
    /// Shorter than the fixed header.
    TooShort,
    /// The four format bytes at offset 1080 are not a known tag.
    BadSignature,
    /// A valid tag for a layout this player does not do (6/8 channels, ...).
    UnsupportedChannels,
    /// The song length is zero or above 128.
    BadSongLength,
    /// An order entry names a pattern the format cannot have (above 127).
    BadOrder,
    /// The pattern data ends before the last pattern the order table uses.
    TruncatedPatterns,
}

/// One instrument. `data` is signed 8-bit PCM, clipped to the bytes present.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    /// The name as stored, NUL-padded. Trackers show these as the song's
    /// instrument list, and musicians write messages in them.
    pub name: [u8; 22],
    pub data: Vec<i8>,
    /// Default volume, `0..=64`.
    pub volume: u8,
    /// Finetune, `-8..=7`.
    pub finetune: i8,
    /// Loop in sample frames; `None` plays once. Always `start < end <= data.len()`.
    pub loop_range: Option<(usize, usize)>,
}

/// One cell of a pattern.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Note {
    /// Amiga period, `0` for none.
    pub period: u16,
    /// Sample number `1..=31`, `0` for none.
    pub sample: u8,
    pub effect: u8,
    pub param: u8,
}

/// A validated module.
#[derive(Clone, Debug)]
pub struct Module {
    pub title: [u8; 20],
    pub samples: Vec<Sample>,
    /// The song's order list (`song_length` entries).
    pub orders: Vec<u8>,
    /// Restart order for looping songs (`0` when the file's value is invalid).
    pub restart: u8,
    pub(crate) patterns: Vec<Note>,
}

impl Module {
    /// Parse and validate a `.mod` file.
    pub fn parse(bytes: &[u8]) -> Result<Module, ModError> {
        crate::parse::parse(bytes)
    }

    /// Number of stored patterns.
    pub fn pattern_count(&self) -> usize {
        self.patterns.len() / (ROWS_PER_PATTERN * CHANNELS)
    }

    /// The cell at `(order, row, channel)`, or an empty note when any index is
    /// out of range (an order entry always names a stored pattern, so this is
    /// belt and braces rather than a path real files take).
    pub fn note(&self, order: usize, row: usize, channel: usize) -> Note {
        let Some(&pattern) = self.orders.get(order) else {
            return Note::default();
        };
        if row >= ROWS_PER_PATTERN || channel >= CHANNELS {
            return Note::default();
        }
        let index = (usize::from(pattern) * ROWS_PER_PATTERN + row) * CHANNELS + channel;
        self.patterns.get(index).copied().unwrap_or_default()
    }
}
