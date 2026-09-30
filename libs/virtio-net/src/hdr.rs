//! The `virtio_net_hdr` that precedes every frame (virtio 1.2, 5.1.6).
//!
//! With `VERSION_1` the header is 12 bytes (it carries `num_buffers`) whether or
//! not `MRG_RXBUF` was negotiated. All fields are little endian.

/// Bytes of the packet header.
pub const HDR_LEN: usize = 12;

/// `flags` bit: `csum_start`/`csum_offset` are valid (transmit) .
pub const F_NEEDS_CSUM: u8 = 1;
/// `flags` bit: the checksum was verified (receive, only with `GUEST_CSUM`).
pub const F_DATA_VALID: u8 = 2;
/// `flags` bit: receive-segment-coalescing info is valid.
pub const F_RSC_INFO: u8 = 4;

/// `gso_type` values.
pub const GSO_NONE: u8 = 0;

/// A decoded packet header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
    pub num_buffers: u16,
}

impl NetHdr {
    /// The header for a plain frame: no offloads, one buffer. What the driver
    /// puts in front of every transmitted frame.
    pub const PLAIN: NetHdr = NetHdr {
        flags: 0,
        gso_type: GSO_NONE,
        hdr_len: 0,
        gso_size: 0,
        csum_start: 0,
        csum_offset: 0,
        num_buffers: 1,
    };

    pub fn encode(&self) -> [u8; HDR_LEN] {
        let mut out = [0; HDR_LEN];
        out[0] = self.flags;
        out[1] = self.gso_type;
        out[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        out[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        out[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        out[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
        out[10..12].copy_from_slice(&self.num_buffers.to_le_bytes());
        out
    }

    /// Decode the first [`HDR_LEN`] bytes of `bytes`; `None` when fewer.
    pub fn decode(bytes: &[u8]) -> Option<NetHdr> {
        let b = bytes.get(..HDR_LEN)?;
        let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        Some(NetHdr {
            flags: b[0],
            gso_type: b[1],
            hdr_len: u16_at(2),
            gso_size: u16_at(4),
            csum_start: u16_at(6),
            csum_offset: u16_at(8),
            num_buffers: u16_at(10),
        })
    }

    /// Whether this header describes an ordinary frame with nothing for the
    /// driver to act on. With no offload negotiated a conforming device only
    /// ever sends this; anything else is a device bug (or a hostile device) and
    /// the frame is dropped and counted. `num_buffers` is not checked: it only
    /// means something with `MRG_RXBUF`.
    pub fn is_plain(&self) -> bool {
        self.flags == 0 && self.gso_type == GSO_NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_the_spec() {
        let hdr = NetHdr {
            flags: 1,
            gso_type: 2,
            hdr_len: 0x0403,
            gso_size: 0x0605,
            csum_start: 0x0807,
            csum_offset: 0x0A09,
            num_buffers: 0x0C0B,
        };
        assert_eq!(hdr.encode(), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(NetHdr::decode(&hdr.encode()), Some(hdr));
        assert_eq!(HDR_LEN, 12);
    }

    #[test]
    fn plain_header_is_one_buffer_and_no_offload() {
        let bytes = NetHdr::PLAIN.encode();
        assert_eq!(bytes, [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0]);
        assert!(NetHdr::PLAIN.is_plain());
    }

    #[test]
    fn short_input_is_refused() {
        for len in 0..HDR_LEN {
            assert_eq!(NetHdr::decode(&[0; 12][..len]), None, "{len}");
        }
        // Trailing frame bytes are ignored.
        assert!(NetHdr::decode(&[0; 100]).is_some());
    }

    #[test]
    fn offload_flags_make_a_header_non_plain() {
        for flags in [F_NEEDS_CSUM, F_DATA_VALID, F_RSC_INFO, 0xFF] {
            assert!(!NetHdr {
                flags,
                ..NetHdr::PLAIN
            }
            .is_plain());
        }
        for gso_type in [1u8, 3, 4, 0x80, 0xFF] {
            assert!(!NetHdr {
                gso_type,
                ..NetHdr::PLAIN
            }
            .is_plain());
        }
        assert!(NetHdr {
            num_buffers: 7,
            ..NetHdr::PLAIN
        }
        .is_plain());
    }
}
