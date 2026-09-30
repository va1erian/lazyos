//! Pings: the echo socket, the table of outstanding requests and the
//! matching of replies to it (see the module docs of [`super`] for the rules).

use alloc::vec;
use alloc::vec::Vec;

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::socket::icmp;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, Ipv4Address};

use super::*;

/// Why a ping could not be started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingError {
    /// The destination or payload is not acceptable.
    BadArgument,
    /// No address yet.
    NoAddress,
    /// The destination is off-link and there is no default route.
    NoRoute,
    /// [`MAX_PINGS`] are already outstanding, or the socket is full.
    Busy,
}

/// The end of one ping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PingOutcome {
    Reply {
        rtt_ms: u32,
        source: [u8; 4],
        bytes: u32,
    },
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PingResult {
    /// The token `ping` returned.
    pub seq: u16,
    pub outcome: PingOutcome,
}

pub(super) struct Pending {
    pub(super) seq: u16,
    pub(super) dst: [u8; 4],
    pub(super) payload_len: usize,
    pub(super) sent_ms: i64,
    pub(super) deadline_ms: i64,
}

fn payload_byte(i: usize) -> u8 {
    (i as u8) ^ 0xA5
}

/// The echo socket, bound to `ident`.
pub(super) fn icmp_socket(ident: u16) -> icmp::Socket<'static> {
    let mut socket = icmp::Socket::new(
        icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY; 8], vec![0; 8 * 1536]),
        icmp::PacketBuffer::new(
            vec![icmp::PacketMetadata::EMPTY; MAX_PINGS],
            vec![0; MAX_PINGS * 1536],
        ),
    );
    let _ = socket.bind(icmp::Endpoint::Ident(ident));
    socket
}

impl Stack {
    /// Start one echo request to `dst`; returns the token that identifies its
    /// result. `timeout_ms` counts from `now_ms`.
    pub fn ping(
        &mut self,
        dst: [u8; 4],
        payload_len: usize,
        timeout_ms: u64,
        now_ms: i64,
    ) -> Result<u16, PingError> {
        if payload_len > MAX_PING_PAYLOAD || !is_usable_unicast(dst) || dst == [255; 4] {
            return Err(PingError::BadArgument);
        }
        let Some(addr) = self.state.addr else {
            return Err(PingError::NoAddress);
        };
        let on_link = crate::config::same_subnet(dst, addr, self.state.prefix_len);
        if !on_link && self.state.gateway.is_none() {
            return Err(PingError::NoRoute);
        }
        if self.pending.len() >= MAX_PINGS {
            return Err(PingError::Busy);
        }
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let data: Vec<u8> = (0..payload_len).map(payload_byte).collect();
        let repr = Icmpv4Repr::EchoRequest {
            ident: self.ident,
            seq_no: seq,
            data: &data,
        };
        let mut buf = vec![0u8; repr.buffer_len()];
        repr.emit(
            &mut Icmpv4Packet::new_unchecked(&mut buf),
            &ChecksumCapabilities::default(),
        );
        let socket = self.sockets.get_mut::<icmp::Socket>(self.icmp);
        socket
            .send_slice(&buf, IpAddress::Ipv4(Ipv4Address::from(dst)))
            .map_err(|_| PingError::Busy)?;
        self.counters.pings_sent += 1;
        self.pending.push(Pending {
            seq,
            dst,
            payload_len,
            sent_ms: now_ms,
            deadline_ms: now_ms + timeout_ms as i64,
        });
        Ok(seq)
    }

    /// Give up on a ping (its caller went away).
    pub fn cancel_ping(&mut self, seq: u16) {
        let before = self.pending.len();
        self.pending.retain(|p| p.seq != seq);
        if self.pending.len() != before {
            self.reset_icmp();
        }
    }

    /// Replace the echo socket with a fresh one.
    ///
    /// smoltcp keeps a packet at the head of a socket's transmit queue until
    /// it can be sent, and a request to an on-link address nobody answers ARP
    /// for never can be, so one dead ping would hold every later ping behind
    /// it. Dropping the socket drops what is queued in it; the pings whose
    /// requests had already left are unaffected (their replies find the new
    /// socket, which has the same identifier), and the ones whose requests had
    /// not simply time out.
    pub(super) fn reset_icmp(&mut self) {
        drop(self.sockets.remove(self.icmp));
        self.icmp = self.sockets.add(icmp_socket(self.ident));
    }

    pub(super) fn handle_icmp(&mut self, now_ms: i64) {
        let socket = self.sockets.get_mut::<icmp::Socket>(self.icmp);
        // Bounded: the receive buffer holds at most eight packets.
        while socket.can_recv() {
            let Ok((data, from)) = socket.recv() else {
                break;
            };
            let IpAddress::Ipv4(from) = from;
            let Ok(packet) = Icmpv4Packet::new_checked(data) else {
                continue;
            };
            let Ok(Icmpv4Repr::EchoReply {
                ident,
                seq_no,
                data,
            }) = Icmpv4Repr::parse(&packet, &ChecksumCapabilities::default())
            else {
                continue;
            };
            if ident != self.ident {
                continue;
            }
            let from = octets(from);
            let Some(at) = self
                .pending
                .iter()
                .position(|p| p.seq == seq_no && p.dst == from)
            else {
                continue;
            };
            // The reply must carry exactly what was sent.
            let sent = &self.pending[at];
            if data.len() != sent.payload_len
                || !data.iter().enumerate().all(|(i, b)| *b == payload_byte(i))
            {
                continue;
            }
            let done = self.pending.remove(at);
            self.counters.pings_answered += 1;
            self.results.push(PingResult {
                seq: done.seq,
                outcome: PingOutcome::Reply {
                    rtt_ms: (now_ms - done.sent_ms).clamp(0, i64::from(u32::MAX)) as u32,
                    source: from,
                    bytes: data.len() as u32,
                },
            });
        }
        let mut expired = false;
        let mut i = 0;
        while i < self.pending.len() {
            if now_ms >= self.pending[i].deadline_ms {
                let gone = self.pending.remove(i);
                self.counters.pings_timed_out += 1;
                self.results.push(PingResult {
                    seq: gone.seq,
                    outcome: PingOutcome::TimedOut,
                });
                expired = true;
            } else {
                i += 1;
            }
        }
        if expired {
            self.reset_icmp();
        }
    }

    /// Finished pings since the last call.
    pub fn take_ping_results(&mut self) -> Vec<PingResult> {
        core::mem::take(&mut self.results)
    }

    /// Pings still waiting.
    pub fn pings_outstanding(&self) -> usize {
        self.pending.len()
    }
}
