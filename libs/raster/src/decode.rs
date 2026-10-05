//! Reading a PWG Raster stream back, for tests and the fake printer's
//! verdict. Bounded: the pages' pixels may not exceed a caller's budget.

use alloc::vec::Vec;

use crate::header::{Header, HEADER_LEN};
use crate::SYNC;

/// One decoded page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub header: Header,
    /// `height` rows of `bytes_per_line` bytes.
    pub pixels: Vec<u8>,
}

/// Why bytes are not a PWG Raster stream this crate can read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// No `RaS2` sync word.
    BadSync,
    /// The bytes end inside a header or a page's rows.
    Truncated,
    /// A header with a colour space or layout this crate does not write.
    BadHeader,
    /// The pages need more than the budget allows.
    TooLarge,
    /// A run or line repeat crosses the end of its line or page.
    Overrun,
}

/// Decodes every page of `bytes`, refusing pages whose pixels would take more
/// than `budget` bytes in all.
pub fn decode(bytes: &[u8], budget: usize) -> Result<Vec<Page>, DecodeError> {
    let rest = bytes.strip_prefix(SYNC).ok_or(DecodeError::BadSync)?;
    let mut at = 0;
    let mut used = 0usize;
    let mut pages = Vec::new();
    while at < rest.len() {
        let head: &[u8; HEADER_LEN] = rest
            .get(at..at + HEADER_LEN)
            .and_then(|h| h.try_into().ok())
            .ok_or(DecodeError::Truncated)?;
        at += HEADER_LEN;
        let header = Header::from_bytes(head)
            .filter(|h| h.width > 0)
            .ok_or(DecodeError::BadHeader)?;
        let size = header
            .bytes_per_line()
            .checked_mul(header.height as usize)
            .ok_or(DecodeError::TooLarge)?;
        used = used.checked_add(size).ok_or(DecodeError::TooLarge)?;
        if used > budget {
            return Err(DecodeError::TooLarge);
        }
        let mut pixels = Vec::with_capacity(size);
        at += rows(&rest[at..], &header, &mut pixels)?;
        pages.push(Page { header, pixels });
    }
    Ok(pages)
}

/// Decodes one page's rows into `pixels`; returns the bytes consumed.
fn rows(input: &[u8], header: &Header, pixels: &mut Vec<u8>) -> Result<usize, DecodeError> {
    let line_len = header.bytes_per_line();
    let bpp = header.color.bytes();
    let mut at = 0;
    let mut row = 0u32;
    let mut line = Vec::with_capacity(line_len);
    let byte = |at: &mut usize| -> Result<u8, DecodeError> {
        let b = *input.get(*at).ok_or(DecodeError::Truncated)?;
        *at += 1;
        Ok(b)
    };
    while row < header.height {
        let repeat = u32::from(byte(&mut at)?) + 1;
        if repeat > header.height - row {
            return Err(DecodeError::Overrun);
        }
        line.clear();
        while line.len() < line_len {
            let control = byte(&mut at)?;
            let (count, literal) = if control < 128 {
                (usize::from(control) + 1, false)
            } else {
                (257 - usize::from(control), true)
            };
            let len = count * bpp;
            if line.len() + len > line_len {
                return Err(DecodeError::Overrun);
            }
            let take = if literal { len } else { bpp };
            let data = input.get(at..at + take).ok_or(DecodeError::Truncated)?;
            at += take;
            if literal {
                line.extend_from_slice(data);
            } else {
                for _ in 0..count {
                    line.extend_from_slice(data);
                }
            }
        }
        for _ in 0..repeat {
            pixels.extend_from_slice(&line);
        }
        row += repeat;
    }
    Ok(at)
}
