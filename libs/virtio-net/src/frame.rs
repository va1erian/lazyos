//! The frame-length policy and receive completion parsing.
//!
//! The driver never parses a payload, but it does enforce two things about
//! every frame in either direction: it is at least an Ethernet header long, and
//! it is no longer than the MTU plus that header. A frame outside those bounds
//! is dropped and counted, never truncated or padded
//! (`docs/networking-plan.md` section 5, data path).

use crate::hdr::{NetHdr, HDR_LEN};
use crate::ETH_HEADER;

/// How a frame length measures against the policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameClass {
    /// Acceptable.
    Ok,
    /// Shorter than an Ethernet header.
    Runt,
    /// Longer than `max_frame`.
    Oversize,
}

/// Classify a frame of `len` bytes against `max_frame` (MTU + 14).
pub const fn classify(len: usize, max_frame: usize) -> FrameClass {
    if len < ETH_HEADER {
        FrameClass::Runt
    } else if len > max_frame {
        FrameClass::Oversize
    } else {
        FrameClass::Ok
    }
}

/// Why a completed receive buffer did not yield a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxError {
    /// The device wrote fewer bytes than a packet header.
    NoHeader,
    /// The device claims to have written more than the buffer holds.
    Overrun,
    /// The header asks for something no negotiated feature allows (an
    /// offload flag or a segmentation type).
    NotPlain,
    /// The frame is shorter than an Ethernet header.
    Runt,
    /// The frame is longer than `max_frame`.
    Oversize,
}

impl RxError {
    /// Whether the counter for this error is the runt one, the oversize one,
    /// or the general "device misbehaved" one.
    pub fn is_length_error(self) -> bool {
        matches!(self, RxError::Runt | RxError::Oversize)
    }
}

/// The frame inside a completed receive buffer. `buf` is the whole slot and
/// `written` the length the device reported in the used ring; both the header
/// and the length are checked, and the returned slice is a subslice of `buf`.
pub fn rx_frame(buf: &[u8], written: u32, max_frame: usize) -> Result<&[u8], RxError> {
    let written = written as usize;
    if written > buf.len() {
        return Err(RxError::Overrun);
    }
    if written < HDR_LEN {
        return Err(RxError::NoHeader);
    }
    let hdr = NetHdr::decode(buf).ok_or(RxError::NoHeader)?;
    if !hdr.is_plain() {
        return Err(RxError::NotPlain);
    }
    let frame = &buf[HDR_LEN..written];
    match classify(frame.len(), max_frame) {
        FrameClass::Ok => Ok(frame),
        FrameClass::Runt => Err(RxError::Runt),
        FrameClass::Oversize => Err(RxError::Oversize),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_FRAME;
    use std::vec;

    #[test]
    fn the_length_boundaries() {
        assert_eq!(classify(0, MAX_FRAME), FrameClass::Runt);
        assert_eq!(classify(13, MAX_FRAME), FrameClass::Runt);
        assert_eq!(classify(14, MAX_FRAME), FrameClass::Ok);
        assert_eq!(classify(60, MAX_FRAME), FrameClass::Ok);
        assert_eq!(
            classify(1514, MAX_FRAME),
            FrameClass::Ok,
            "exactly MTU + 14"
        );
        assert_eq!(
            classify(1515, MAX_FRAME),
            FrameClass::Oversize,
            "one byte over"
        );
        assert_eq!(classify(usize::MAX, MAX_FRAME), FrameClass::Oversize);
        assert_eq!(MAX_FRAME, 1514);
        // A lower MTU moves the bound with it.
        assert_eq!(classify(590, 576 + 14), FrameClass::Ok);
        assert_eq!(classify(591, 576 + 14), FrameClass::Oversize);
    }

    fn slot(frame_len: usize) -> vec::Vec<u8> {
        let mut buf = vec![0xEE; 2048];
        buf[..HDR_LEN].copy_from_slice(&NetHdr::PLAIN.encode());
        for (i, b) in buf[HDR_LEN..HDR_LEN + frame_len].iter_mut().enumerate() {
            *b = i as u8;
        }
        buf
    }

    #[test]
    fn a_good_completion_yields_exactly_the_frame() {
        for len in [14usize, 15, 60, 1500, 1514] {
            let buf = slot(len);
            let frame = rx_frame(&buf, (HDR_LEN + len) as u32, MAX_FRAME).unwrap();
            assert_eq!(frame.len(), len);
            assert_eq!(frame[0], 0);
            assert_eq!(frame[len - 1], (len - 1) as u8);
        }
    }

    #[test]
    fn short_and_long_completions_are_classified() {
        let buf = slot(1600);
        assert_eq!(rx_frame(&buf, 0, MAX_FRAME), Err(RxError::NoHeader));
        assert_eq!(rx_frame(&buf, 11, MAX_FRAME), Err(RxError::NoHeader));
        assert_eq!(
            rx_frame(&buf, 12, MAX_FRAME),
            Err(RxError::Runt),
            "header only, empty frame"
        );
        assert_eq!(
            rx_frame(&buf, 25, MAX_FRAME),
            Err(RxError::Runt),
            "13-byte frame"
        );
        assert!(rx_frame(&buf, 26, MAX_FRAME).is_ok(), "14-byte frame");
        assert!(rx_frame(&buf, 12 + 1514, MAX_FRAME).is_ok());
        assert_eq!(rx_frame(&buf, 12 + 1515, MAX_FRAME), Err(RxError::Oversize));
        assert_eq!(rx_frame(&buf, 2048, MAX_FRAME), Err(RxError::Oversize));
    }

    #[test]
    fn a_length_beyond_the_buffer_is_an_overrun_not_a_slice_panic() {
        let buf = slot(100);
        assert_eq!(rx_frame(&buf, 2049, MAX_FRAME), Err(RxError::Overrun));
        assert_eq!(rx_frame(&buf, u32::MAX, MAX_FRAME), Err(RxError::Overrun));
        assert_eq!(rx_frame(&[], 0, MAX_FRAME), Err(RxError::NoHeader));
        assert_eq!(rx_frame(&[], 1, MAX_FRAME), Err(RxError::Overrun));
    }

    #[test]
    fn an_offload_header_is_dropped() {
        let mut buf = slot(60);
        buf[0] = 2; // DATA_VALID, but no GUEST_CSUM was negotiated
        assert_eq!(rx_frame(&buf, 72, MAX_FRAME), Err(RxError::NotPlain));
        buf[0] = 0;
        buf[1] = 1; // TCPv4 GSO
        assert_eq!(rx_frame(&buf, 72, MAX_FRAME), Err(RxError::NotPlain));
    }

    #[test]
    fn length_errors_are_told_apart() {
        assert!(RxError::Runt.is_length_error());
        assert!(RxError::Oversize.is_length_error());
        assert!(!RxError::Overrun.is_length_error());
        assert!(!RxError::NotPlain.is_length_error());
    }
}
