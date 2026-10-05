//! Compressing rows as they arrive.
//!
//! Each line is a repeat byte (the line occurs `n + 1` times in a row, up to
//! 256) and the line's pixels as runs: a byte `n` in `0..=127` is one pixel
//! repeated `n + 1` times; a byte `257 - n` in `129..=255` is `n` (2 to 128)
//! pixels given as they are. A white document page is mostly repeated white
//! lines, a few bytes per 256 rows.

use alloc::vec::Vec;

use crate::header::{ColorSpace, Header};

/// Why rows could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// A row is not `width` RGBA pixels (or `bytes_per_line` bytes).
    RowLength,
    /// More rows than the header's height.
    TooManyRows,
    /// [`PageEncoder::finish`] before every row was given.
    MissingRows { given: u32, height: u32 },
}

/// Longest run in either form, pixels.
const MAX_RUN: usize = 128;
/// Most lines one repeat byte covers.
const MAX_REPEAT: u32 = 256;

/// One page being encoded: the header, then rows in order.
pub struct PageEncoder {
    header: Header,
    out: Vec<u8>,
    /// The last row given, in the page's colour space, not yet written.
    pending: Vec<u8>,
    /// How many times `pending` occurs in a row (0 before the first row).
    repeats: u32,
    rows: u32,
    /// A scratch row for colour conversion.
    scratch: Vec<u8>,
}

impl PageEncoder {
    /// Starts a page: its header is the first output.
    pub fn new(header: Header) -> PageEncoder {
        let out = header.to_bytes();
        let line = header.bytes_per_line();
        PageEncoder {
            header,
            out,
            pending: Vec::with_capacity(line),
            repeats: 0,
            rows: 0,
            scratch: Vec::with_capacity(line),
        }
    }

    /// The page's header.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Adds the next row given as opaque RGBA pixels (`width * 4` bytes),
    /// converted to the page's colour space. Grey is the luma of sRGB
    /// (Rec. 709 weights), which is how the screen's white and black come out
    /// as paper and full ink.
    pub fn push_rgba(&mut self, rgba: &[u8]) -> Result<(), EncodeError> {
        if rgba.len() != self.header.width as usize * 4 {
            return Err(EncodeError::RowLength);
        }
        let mut row = core::mem::take(&mut self.scratch);
        row.clear();
        match self.header.color {
            ColorSpace::Srgb8 => {
                for p in rgba.as_chunks::<4>().0 {
                    row.extend_from_slice(&p[..3]);
                }
            }
            ColorSpace::Sgray8 => row.extend(
                rgba.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|p| luma(p[0], p[1], p[2])),
            ),
        }
        let result = self.push_row(&row);
        self.scratch = row;
        result
    }

    /// Adds the next row already in the page's colour space
    /// (`bytes_per_line` bytes).
    pub fn push_row(&mut self, row: &[u8]) -> Result<(), EncodeError> {
        if row.len() != self.header.bytes_per_line() {
            return Err(EncodeError::RowLength);
        }
        if self.rows >= self.header.height {
            return Err(EncodeError::TooManyRows);
        }
        self.rows += 1;
        if self.repeats > 0 && self.repeats < MAX_REPEAT && self.pending == row {
            self.repeats += 1;
            return Ok(());
        }
        self.flush_line();
        self.pending.clear();
        self.pending.extend_from_slice(row);
        self.repeats = 1;
        Ok(())
    }

    /// The bytes encoded so far, handed over (the last row given is held back
    /// in case the next one repeats it).
    pub fn take_output(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.out)
    }

    /// Ends the page and returns the rest of its bytes.
    pub fn finish(mut self) -> Result<Vec<u8>, EncodeError> {
        if self.rows != self.header.height {
            return Err(EncodeError::MissingRows {
                given: self.rows,
                height: self.header.height,
            });
        }
        self.flush_line();
        Ok(self.out)
    }

    /// Writes `pending` with its repeat count.
    fn flush_line(&mut self) {
        if self.repeats == 0 {
            return;
        }
        self.out.push((self.repeats - 1) as u8);
        compress(&mut self.out, &self.pending, self.header.color.bytes());
        self.repeats = 0;
    }
}

/// Grey from sRGB: Rec. 709 luma in 8.8 fixed point (weights sum to 256).
fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((54 * u32::from(r) + 183 * u32::from(g) + 19 * u32::from(b) + 128) >> 8) as u8
}

/// Appends `line`'s pixels (`bpp` bytes each) as runs.
fn compress(out: &mut Vec<u8>, line: &[u8], bpp: usize) {
    let n = line.len() / bpp;
    let px = |i: usize| &line[i * bpp..(i + 1) * bpp];
    let mut i = 0;
    while i < n {
        let mut run = 1;
        while i + run < n && run < MAX_RUN && px(i + run) == px(i) {
            run += 1;
        }
        if run > 1 || i + 1 == n {
            out.push((run - 1) as u8);
            out.extend_from_slice(px(i));
            i += run;
            continue;
        }
        // Literals: up to the next pair of equal pixels, which starts a run.
        let mut count = 1;
        while i + count < n
            && count < MAX_RUN
            && !(i + count + 1 < n && px(i + count) == px(i + count + 1))
        {
            count += 1;
        }
        if count == 1 {
            out.push(0);
        } else {
            out.push((257 - count) as u8);
        }
        out.extend_from_slice(&line[i * bpp..(i + count) * bpp]);
        i += count;
    }
}
