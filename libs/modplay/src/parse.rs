//! `.mod` header, order table, pattern and sample parsing.

use alloc::vec::Vec;

use crate::module::{ModError, Module, Note, Sample, CHANNELS, ROWS_PER_PATTERN};

const SAMPLES: usize = 31;
const SAMPLE_HEADER: usize = 30;
const TITLE: usize = 20;
const ORDERS_AT: usize = TITLE + SAMPLES * SAMPLE_HEADER + 2;
const SIGNATURE_AT: usize = ORDERS_AT + 128;
const HEADER: usize = SIGNATURE_AT + 4;
const PATTERN_BYTES: usize = ROWS_PER_PATTERN * CHANNELS * 4;

fn be16(bytes: &[u8], at: usize) -> usize {
    usize::from(u16::from_be_bytes([bytes[at], bytes[at + 1]]))
}

/// Classify the format tag: `Ok` for the four-channel tags.
fn check_signature(tag: &[u8]) -> Result<(), ModError> {
    match tag {
        b"M.K." | b"M!K!" | b"4CHN" | b"FLT4" => Ok(()),
        // Real formats with another channel layout (`6CHN`, `16CN`, `32CH`, ...).
        b"FLT8" | b"OCTA" | b"CD81" | b"OKTA" => Err(ModError::UnsupportedChannels),
        [d, b'C', b'H', b'N'] if d.is_ascii_digit() => Err(ModError::UnsupportedChannels),
        [a, b, b'C', b'H' | b'N'] if a.is_ascii_digit() && b.is_ascii_digit() => {
            Err(ModError::UnsupportedChannels)
        }
        _ => Err(ModError::BadSignature),
    }
}

/// `(length, finetune, volume, loop start, loop length)`, lengths in bytes.
fn sample_header(bytes: &[u8], index: usize) -> (usize, i8, u8, usize, usize) {
    let at = TITLE + index * SAMPLE_HEADER + 22;
    // Sign-extend the finetune nibble.
    let finetune = (((bytes[at + 2] & 0x0F) << 4) as i8) >> 4;
    (
        be16(bytes, at) * 2,
        finetune,
        bytes[at + 3].min(64),
        be16(bytes, at + 4) * 2,
        be16(bytes, at + 6) * 2,
    )
}

fn parse_note(cell: &[u8; 4]) -> Note {
    Note {
        period: (u16::from(cell[0] & 0x0F) << 8) | u16::from(cell[1]),
        sample: (cell[0] & 0xF0) | (cell[2] >> 4),
        effect: cell[2] & 0x0F,
        param: cell[3],
    }
}

/// Parse and validate `bytes`.
pub fn parse(bytes: &[u8]) -> Result<Module, ModError> {
    if bytes.len() < HEADER {
        return Err(ModError::TooShort);
    }
    check_signature(&bytes[SIGNATURE_AT..HEADER])?;

    let song_length = usize::from(bytes[ORDERS_AT - 2]);
    if song_length == 0 || song_length > 128 {
        return Err(ModError::BadSongLength);
    }
    let restart = bytes[ORDERS_AT - 1];
    // The whole 128-entry table sizes the pattern store, as other players do,
    // but a value above 127 can never be a real pattern.
    let table = &bytes[ORDERS_AT..SIGNATURE_AT];
    if table.iter().any(|&p| p > 127) {
        return Err(ModError::BadOrder);
    }
    let pattern_count = usize::from(table.iter().copied().max().unwrap_or(0)) + 1;

    let patterns_end = HEADER + pattern_count * PATTERN_BYTES;
    if bytes.len() < patterns_end {
        return Err(ModError::TruncatedPatterns);
    }
    let patterns: Vec<Note> = bytes[HEADER..patterns_end]
        .as_chunks::<4>()
        .0
        .iter()
        .map(parse_note)
        .collect();

    // Sample data follows the patterns back to back. Files in the wild are
    // often truncated, so each sample is clipped to the bytes present.
    let mut cursor = patterns_end;
    let mut samples = Vec::with_capacity(SAMPLES);
    for index in 0..SAMPLES {
        let (len, finetune, volume, loop_start, loop_len) = sample_header(bytes, index);
        let end = cursor.saturating_add(len).min(bytes.len());
        let data: Vec<i8> = bytes[cursor..end].iter().map(|&b| b as i8).collect();
        cursor = end;
        // A loop of one word (2 bytes) or less means "no loop".
        let loop_range = if loop_len > 2 && loop_start < data.len() {
            Some((
                loop_start,
                loop_start.saturating_add(loop_len).min(data.len()),
            ))
        } else {
            None
        };
        samples.push(Sample {
            data,
            volume,
            finetune,
            loop_range,
        });
    }

    let mut title = [0u8; TITLE];
    title.copy_from_slice(&bytes[..TITLE]);
    Ok(Module {
        title,
        samples,
        orders: table[..song_length].to_vec(),
        restart: if usize::from(restart) < song_length {
            restart
        } else {
            0
        },
        patterns,
    })
}
