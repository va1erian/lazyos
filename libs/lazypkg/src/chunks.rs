//! Entry decompression piece by piece: an entry of any size goes through one
//! window of [`CHUNK`] bytes, so a reader never holds a whole file.
//!
//! `pkgd`'s heap never returns a block larger than the user heap's largest
//! size class (1 MiB), so unpacking a 15 MB program into one buffer grows it
//! by 15 MB until the service restarts. Streaming through the window bounds
//! the cost of an entry at one window, whatever the entry's size. Size and
//! CRC-32 are checked as in [`crate::inflate`], against the central
//! directory, once the last piece is out; the cap is checked before each
//! piece is handed over, so nothing past the declared size is ever emitted.

use alloc::boxed::Box;
use alloc::vec;
use crc::{Crc, CRC_32_ISO_HDLC};
use miniz_oxide::inflate::core::{decompress, DecompressorOxide};
use miniz_oxide::inflate::TINFLStatus;

use crate::inflate::InflateError;

/// Largest piece handed to a sink (1 MiB): a power of two, as a wrapping
/// inflate window must be, at least DEFLATE's 32 KiB history, and equal to
/// both the user heap's largest recycled block and the most one LazyOS
/// append call writes.
pub const CHUNK: usize = 1 << 20;

static CRC: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);

/// Why streaming an entry stopped.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Halt<E> {
    Inflate(InflateError),
    /// The sink refused a piece; nothing more was produced.
    Sink(E),
}

/// Inflate (or pass through, for stored) one entry to `sink` in order, in
/// pieces of at most [`CHUNK`] bytes, bounded by `size`; returns the CRC-32
/// of everything emitted. An empty entry emits nothing.
pub(crate) fn decompress_chunks<E>(
    method: u16,
    data: &[u8],
    size: u32,
    sink: &mut dyn FnMut(&[u8]) -> Result<(), E>,
) -> Result<u32, Halt<E>> {
    let expected = size as usize;
    let mut digest = CRC.digest();
    let mut emit = |piece: &[u8]| {
        digest.update(piece);
        sink(piece).map_err(Halt::Sink)
    };
    let produced = match method {
        0 => {
            if data.len() != expected {
                return Err(Halt::Inflate(InflateError::SizeMismatch {
                    expected: size,
                    actual: data.len().min(u32::MAX as usize) as u32,
                }));
            }
            data.chunks(CHUNK).try_for_each(&mut emit)?;
            expected
        }
        8 => inflate_chunks(data, size, &mut emit)?,
        _ => return Err(Halt::Inflate(InflateError::Corrupt)),
    };
    if produced != expected {
        return Err(Halt::Inflate(InflateError::SizeMismatch {
            expected: size,
            actual: produced.min(u32::MAX as usize) as u32,
        }));
    }
    Ok(digest.finalize())
}

/// Raw DEFLATE through a wrapping window: each call fills the window from
/// where the last one stopped, back-references read the history behind it,
/// and the new bytes go to `emit` before the window wraps over them.
fn inflate_chunks<E>(
    data: &[u8],
    size: u32,
    emit: &mut impl FnMut(&[u8]) -> Result<(), Halt<E>>,
) -> Result<usize, Halt<E>> {
    let corrupt = || Halt::Inflate(InflateError::Corrupt);
    let expected = size as usize;
    let mut window = vec![0u8; CHUNK];
    // About 11 KiB: boxed so it never lands on a small stack.
    let mut state = Box::<DecompressorOxide>::default();
    let (mut read_total, mut out_pos, mut produced) = (0usize, 0usize, 0usize);
    loop {
        let input = data.get(read_total..).ok_or_else(corrupt)?;
        // No flags: the whole input is here, and the window wraps.
        let (status, read, written) = decompress(&mut state, input, &mut window, out_pos, 0);
        read_total += read;
        produced += written;
        if produced > expected {
            // The stream wants to write past the declared size.
            return Err(Halt::Inflate(InflateError::SizeMismatch {
                expected: size,
                actual: u32::MAX,
            }));
        }
        if written > 0 {
            emit(&window[out_pos..out_pos + written])?;
        }
        out_pos = (out_pos + written) & (CHUNK - 1);
        match status {
            TINFLStatus::Done => return Ok(produced),
            TINFLStatus::HasMoreOutput => {}
            _ => return Err(corrupt()),
        }
    }
}
