//! Packet framing for the USB Bluetooth transport ("H2", `docs/bluetooth-plan.md`
//! phase B2), shared by `usbd` and host tests.
//!
//! `usbd` binds a controller's interface 0 (class `E0/01/01`) and moves its
//! packets without knowing Bluetooth: HCI commands go out as class requests,
//! events come in on the interrupt endpoint and ACL data on the bulk
//! endpoints. An endpoint delivers *chunks* (at most `wMaxPacketSize` bytes),
//! and a packet longer than a chunk arrives in several, so the only thing this
//! crate reads is what delimits a packet: the HCI event's length byte and the
//! ACL header's length field. Everything else in a packet is `btd`'s to parse,
//! because a stranger in radio range wrote it.
//!
//! **The device is hostile.** Reassembly is bounded: a packet longer than the
//! buffer is skipped (counted), never grown into; the loops are bounded by the
//! input; nothing here panics on any byte sequence.
//!
//! Packets travel between `usbd` and `btd` as ring frames: one byte telling
//! the kind ([`PACKET_COMMAND`], [`PACKET_ACL`], [`PACKET_EVENT`], the H4
//! indicators) and the packet.

#![no_std]

/// H4 packet indicator of an HCI command (host to controller).
pub const PACKET_COMMAND: u8 = 0x01;
/// H4 packet indicator of ACL data (either direction).
pub const PACKET_ACL: u8 = 0x02;
/// H4 packet indicator of an HCI event (controller to host).
pub const PACKET_EVENT: u8 = 0x04;

/// Bytes of an HCI event header: event code and parameter length.
pub const EVENT_HEADER: usize = 2;
/// Longest HCI event: header and 255 parameter bytes.
pub const MAX_EVENT: usize = EVENT_HEADER + 255;
/// Bytes of an HCI command header: opcode and parameter length.
pub const COMMAND_HEADER: usize = 3;
/// Longest HCI command: header and 255 parameter bytes.
pub const MAX_COMMAND: usize = COMMAND_HEADER + 255;
/// Bytes of an ACL header: handle and flags, data length.
pub const ACL_HEADER: usize = 4;
/// Longest ACL packet carried (header and data). Controllers' buffers are far
/// smaller (1021 data bytes is typical); a larger claim is a broken or
/// hostile device and is skipped.
pub const MAX_ACL: usize = ACL_HEADER + 2000;

/// What a [`Reassembler`] is cutting packets of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Event,
    Acl,
}

impl Kind {
    const fn header(self) -> usize {
        match self {
            Kind::Event => EVENT_HEADER,
            Kind::Acl => ACL_HEADER,
        }
    }

    const fn capacity(self) -> usize {
        match self {
            Kind::Event => MAX_EVENT,
            Kind::Acl => MAX_ACL,
        }
    }

    /// The whole packet's length, from its complete header.
    fn total(self, header: &[u8]) -> usize {
        match self {
            Kind::Event => EVENT_HEADER + usize::from(header[1]),
            Kind::Acl => ACL_HEADER + usize::from(u16::from_le_bytes([header[2], header[3]])),
        }
    }
}

/// Cuts a stream of endpoint chunks into whole packets.
pub struct Reassembler {
    kind: Kind,
    buf: [u8; MAX_ACL],
    len: usize,
    /// Bytes of an oversized packet still to throw away.
    skip: usize,
    oversize: u32,
}

impl Reassembler {
    pub const fn new(kind: Kind) -> Reassembler {
        Reassembler {
            kind,
            buf: [0; MAX_ACL],
            len: 0,
            skip: 0,
            oversize: 0,
        }
    }

    /// Packets thrown away for claiming more than the buffer holds.
    pub fn oversize(&self) -> u32 {
        self.oversize
    }

    /// Forget a half-received packet (the endpoint was reset or restarted, so
    /// its remainder will never come).
    pub fn reset(&mut self) {
        self.len = 0;
        self.skip = 0;
    }

    /// Feed one chunk; `deliver` is called with each packet it completes (a
    /// chunk may hold the end of one packet and the start of the next, or
    /// several whole ones).
    pub fn push(&mut self, mut chunk: &[u8], mut deliver: impl FnMut(&[u8])) {
        let header = self.kind.header();
        while !chunk.is_empty() {
            if self.skip != 0 {
                let dropped = self.skip.min(chunk.len());
                self.skip -= dropped;
                chunk = &chunk[dropped..];
                continue;
            }
            // Take up to the header, then up to the packet's end.
            let want = if self.len < header {
                header - self.len
            } else {
                self.kind.total(&self.buf[..header]) - self.len
            };
            let take = want.min(chunk.len());
            self.buf[self.len..self.len + take].copy_from_slice(&chunk[..take]);
            self.len += take;
            chunk = &chunk[take..];
            if self.len < header {
                continue;
            }
            let total = self.kind.total(&self.buf[..header]);
            if total > self.kind.capacity() {
                self.skip = total - self.len;
                self.len = 0;
                self.oversize = self.oversize.saturating_add(1);
            } else if self.len == total {
                deliver(&self.buf[..total]);
                self.len = 0;
            }
        }
    }
}

/// A packet `btd` asked to send, checked and classified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outbound<'a> {
    /// An HCI command: opcode, length and parameters (no indicator).
    Command(&'a [u8]),
    /// An ACL data packet (no indicator).
    Acl(&'a [u8]),
}

/// Why a frame from `btd` was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Empty frame.
    Empty,
    /// Not a command or ACL indicator (events and SCO do not go out).
    Kind,
    /// Shorter than its header, or its length field does not match the frame.
    Length,
}

/// Check a ring frame (`indicator` and packet) from the host stack before it
/// reaches the device: the kind must be one that goes out, and the packet's
/// own length field must account for exactly the bytes sent.
pub fn outbound(frame: &[u8]) -> Result<Outbound<'_>, Refusal> {
    let (&kind, packet) = frame.split_first().ok_or(Refusal::Empty)?;
    match kind {
        PACKET_COMMAND => {
            if packet.len() < COMMAND_HEADER
                || packet.len() != COMMAND_HEADER + usize::from(packet[2])
            {
                return Err(Refusal::Length);
            }
            Ok(Outbound::Command(packet))
        }
        PACKET_ACL => {
            if packet.len() < ACL_HEADER {
                return Err(Refusal::Length);
            }
            let data = usize::from(u16::from_le_bytes([packet[2], packet[3]]));
            if packet.len() != ACL_HEADER + data {
                return Err(Refusal::Length);
            }
            Ok(Outbound::Acl(packet))
        }
        _ => Err(Refusal::Kind),
    }
}

/// A ring frame for `packet` of `kind`: the indicator and the bytes. `None`
/// when `out` is too small.
pub fn frame(kind: u8, packet: &[u8], out: &mut [u8]) -> Option<usize> {
    let len = packet.len() + 1;
    let out = out.get_mut(..len)?;
    out[0] = kind;
    out[1..].copy_from_slice(packet);
    Some(len)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    fn collect(reassembler: &mut Reassembler, chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for chunk in chunks {
            reassembler.push(chunk, |p| out.push(p.to_vec()));
        }
        out
    }

    /// A Command Complete for Reset (Core spec 7.7.14), 6 bytes.
    const RESET_DONE: [u8; 6] = [0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];

    #[test]
    fn a_whole_event_in_one_chunk() {
        let mut r = Reassembler::new(Kind::Event);
        assert_eq!(collect(&mut r, &[&RESET_DONE]), [RESET_DONE.to_vec()]);
    }

    #[test]
    fn an_event_split_at_every_byte_still_arrives_whole() {
        for cut in 1..RESET_DONE.len() {
            let mut r = Reassembler::new(Kind::Event);
            let got = collect(&mut r, &[&RESET_DONE[..cut], &RESET_DONE[cut..]]);
            assert_eq!(got, [RESET_DONE.to_vec()], "cut at {cut}");
        }
    }

    #[test]
    fn two_events_in_one_chunk_and_one_across_chunks() {
        let mut stream = RESET_DONE.to_vec();
        stream.extend_from_slice(&[0x13, 0x05, 1, 2, 3, 4, 5]);
        let mut r = Reassembler::new(Kind::Event);
        let got = collect(&mut r, &[&stream[..4], &stream[4..9], &stream[9..]]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], RESET_DONE);
        assert_eq!(got[1], [0x13, 0x05, 1, 2, 3, 4, 5]);
    }

    #[test]
    fn an_event_with_no_parameters_is_a_packet() {
        let mut r = Reassembler::new(Kind::Event);
        assert_eq!(collect(&mut r, &[&[0x05, 0x00]]), [std::vec![0x05, 0x00]]);
    }

    #[test]
    fn the_longest_event_fits() {
        let mut event = std::vec![0x3E, 255];
        event.extend((0..255).map(|i| i as u8));
        let mut r = Reassembler::new(Kind::Event);
        let chunks: Vec<&[u8]> = event.chunks(16).collect();
        assert_eq!(collect(&mut r, &chunks), [event.clone()]);
        assert_eq!(r.oversize(), 0);
    }

    #[test]
    fn acl_packets_are_cut_by_the_length_field() {
        let mut acl = std::vec![0x2A, 0x20, 0x08, 0x00];
        acl.extend_from_slice(&[9, 8, 7, 6, 5, 4, 3, 2]);
        let mut r = Reassembler::new(Kind::Acl);
        let chunks: Vec<&[u8]> = acl.chunks(5).collect();
        assert_eq!(collect(&mut r, &chunks), [acl]);
    }

    #[test]
    fn an_acl_packet_with_no_data_is_a_packet() {
        let mut r = Reassembler::new(Kind::Acl);
        let empty = [0x01, 0x00, 0x00, 0x00];
        assert_eq!(collect(&mut r, &[&empty]), [empty.to_vec()]);
    }

    #[test]
    fn an_oversized_acl_claim_is_skipped_and_the_next_packet_survives() {
        // Claims 65535 data bytes: skipped to its (claimed) end.
        let mut stream = std::vec![0x01, 0x00, 0xFF, 0xFF];
        stream.resize(ACL_HEADER + 65535, 0xAA);
        let next = [0x02, 0x00, 0x01, 0x00, 0x77];
        stream.extend_from_slice(&next);
        let mut r = Reassembler::new(Kind::Acl);
        let chunks: Vec<&[u8]> = stream.chunks(512).collect();
        assert_eq!(collect(&mut r, &chunks), [next.to_vec()]);
        assert_eq!(r.oversize(), 1);
    }

    #[test]
    fn reset_drops_a_half_received_packet() {
        let mut r = Reassembler::new(Kind::Event);
        assert!(collect(&mut r, &[&RESET_DONE[..3]]).is_empty());
        r.reset();
        assert_eq!(collect(&mut r, &[&RESET_DONE]), [RESET_DONE.to_vec()]);
    }

    #[test]
    fn no_byte_sequence_panics_or_overruns() {
        // A deterministic scramble through both kinds in odd chunk sizes.
        let mut state = 0x1234_5678u32;
        let mut bytes = Vec::new();
        for _ in 0..20_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            bytes.push((state >> 24) as u8);
        }
        for kind in [Kind::Event, Kind::Acl] {
            for size in [1, 3, 7, 64, 513] {
                let mut r = Reassembler::new(kind);
                for chunk in bytes.chunks(size) {
                    r.push(chunk, |p| assert!(p.len() <= kind.capacity()));
                }
            }
        }
    }

    #[test]
    fn outbound_accepts_exact_commands_and_acl() {
        let reset = [PACKET_COMMAND, 0x03, 0x0C, 0x00];
        assert_eq!(outbound(&reset), Ok(Outbound::Command(&reset[1..])));
        let acl = [PACKET_ACL, 0x01, 0x20, 0x02, 0x00, 0xAA, 0xBB];
        assert_eq!(outbound(&acl), Ok(Outbound::Acl(&acl[1..])));
    }

    #[test]
    fn outbound_refuses_what_does_not_add_up() {
        assert_eq!(outbound(&[]), Err(Refusal::Empty));
        assert_eq!(outbound(&[PACKET_EVENT, 0, 0]), Err(Refusal::Kind));
        assert_eq!(outbound(&[0x03, 0, 0, 0, 0]), Err(Refusal::Kind));
        assert_eq!(
            outbound(&[PACKET_COMMAND, 0x03, 0x0C]),
            Err(Refusal::Length)
        );
        assert_eq!(
            outbound(&[PACKET_COMMAND, 0x03, 0x0C, 0x01]),
            Err(Refusal::Length)
        );
        assert_eq!(
            outbound(&[PACKET_COMMAND, 0x03, 0x0C, 0x00, 0x00]),
            Err(Refusal::Length)
        );
        assert_eq!(outbound(&[PACKET_ACL, 0, 0, 1]), Err(Refusal::Length));
        assert_eq!(outbound(&[PACKET_ACL, 0, 0, 1, 0]), Err(Refusal::Length));
        assert_eq!(outbound(&[PACKET_ACL, 0, 0, 0, 0, 9]), Err(Refusal::Length));
    }

    #[test]
    fn frame_prefixes_the_indicator() {
        let mut out = [0u8; 8];
        assert_eq!(frame(PACKET_EVENT, &RESET_DONE, &mut out), Some(7));
        assert_eq!(
            &out[..7],
            &[PACKET_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]
        );
        assert_eq!(frame(PACKET_EVENT, &RESET_DONE, &mut out[..6]), None);
    }
}
