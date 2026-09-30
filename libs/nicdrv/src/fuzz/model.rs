//! The reference model and the rig that drives the engine beside it.

use std::collections::VecDeque;
use std::vec::Vec;

use framering::{off, ring_bytes, FrameBuf, PushError, HEADER_BYTES, MAX_FRAME, SLOT_BYTES};

use super::script::Script;
use crate::engine::{AttachError, CtlError, EV_LINK_CHANGE, EV_RX_READY, EV_TX_SPACE};
use crate::testdev::bed::{Bed, MAC, OWNER};
use crate::{Fatal, PumpOutcome, Stats};

/// The frame bound at the default MTU (1500 + 14).
pub(super) const POLICY_MAX: usize = 1514;

pub(super) fn pattern(id: u32, len: usize, dst: [u8; 6]) -> Vec<u8> {
    let mut f: Vec<u8> = (0..len)
        .map(|j| (id as usize).wrapping_mul(13).wrapping_add(j) as u8)
        .collect();
    if len >= 6 {
        f[..6].copy_from_slice(&dst);
    }
    f
}

#[derive(Default)]
pub(super) struct Model {
    pub(super) attached: Option<(u64, u32)>,
    pub(super) mode: u32,
    pub(super) link: bool,
    pub(super) link_event: bool,
    /// Frames on the device's receive queue, not yet pumped.
    pub(super) dev_rx: VecDeque<Vec<u8>>,
    pub(super) client_rx: VecDeque<Vec<u8>>,
    pub(super) client_tx: VecDeque<Vec<u8>>,
    /// Frames the device holds (queued, not yet completed by `transmitted`).
    pub(super) dev_tx: VecDeque<Vec<u8>>,
    pub(super) rx_armed: bool,
    pub(super) tx_armed: bool,
    pub(super) stats: Stats,
}

pub(super) struct Run {
    pub(super) bed: Bed,
    pub(super) model: Model,
    pub(super) rx_entries: u16,
    pub(super) tx_entries: u16,
    /// A client scribbled or the device lied: only safety is asserted.
    pub(super) tainted: bool,
    pub(super) frame_id: u32,
    pub(super) last: Stats,
}

pub(super) fn monotonic(before: &Stats, after: &Stats) {
    assert!(after.rx_frames >= before.rx_frames && after.tx_frames >= before.tx_frames);
    assert!(after.rx_bytes >= before.rx_bytes && after.tx_bytes >= before.tx_bytes);
    assert!(after.rx_dropped >= before.rx_dropped && after.tx_dropped >= before.tx_dropped);
    assert!(after.runts >= before.runts && after.oversize >= before.oversize);
    assert!(after.ring_errors >= before.ring_errors && after.interrupts >= before.interrupts);
}

impl Run {
    pub(super) fn client_slots(&self) -> usize {
        self.bed.client.as_ref().map_or(0, |c| c.slots as usize)
    }

    pub(super) fn deliver(&mut self, len: usize, dst: [u8; 6]) {
        self.frame_id += 1;
        let frame = pattern(self.frame_id, len, dst);
        let accepted = self.bed.dev.deliver(&frame);
        if !self.tainted {
            assert_eq!(
                accepted,
                self.model.dev_rx.len() < usize::from(self.rx_entries),
                "a buffer is available exactly when fewer than all are in use"
            );
        }
        if accepted {
            self.model.dev_rx.push_back(frame);
        }
    }

    pub(super) fn client_push(&mut self, len: usize) {
        if self.bed.client.is_none() {
            return;
        }
        self.frame_id += 1;
        let frame = pattern(self.frame_id, len, [0xFF; 6]);
        let result = self.bed.client().tx.push(&frame);
        if self.tainted {
            return;
        }
        let expected = if len == 0 {
            Err(PushError::Empty)
        } else if len > MAX_FRAME {
            Err(PushError::TooLong)
        } else if self.model.client_tx.len() == self.client_slots() {
            Err(PushError::Full)
        } else {
            Ok(())
        };
        assert_eq!(result, expected);
        if result.is_ok() {
            self.model.client_tx.push_back(frame);
        }
    }

    pub(super) fn client_pop(&mut self) {
        if self.bed.client.is_none() {
            return;
        }
        let mut buf: FrameBuf = [0; MAX_FRAME];
        let popped = self.bed.client().rx.pop(&mut buf);
        if self.tainted {
            if let Ok(Some(n)) = popped {
                assert!(n <= MAX_FRAME);
            }
            return;
        }
        match self.model.client_rx.pop_front() {
            Some(frame) => {
                let n = popped
                    .expect("a queued frame pops")
                    .expect("and is delivered");
                assert_eq!(&buf[..n], &frame[..], "frame delivered intact and in order");
            }
            None => assert_eq!(popped, Ok(None)),
        }
    }

    pub(super) fn device_transmit(&mut self) {
        let got = self.bed.dev.transmitted();
        if self.tainted {
            return;
        }
        match self.model.dev_tx.pop_front() {
            Some(frame) => assert_eq!(
                got.as_deref(),
                Some(&frame[..]),
                "the wire carries exactly what was valid"
            ),
            None => assert_eq!(got, None),
        }
    }

    pub(super) fn pump(&mut self) -> Result<(), Fatal> {
        let outcome = self.bed.engine.pump(&mut self.bed.dev)?;
        let stats = *self.bed.engine.stats();
        monotonic(&self.last, &stats);
        self.last = stats;
        assert!(outcome.events & !(EV_RX_READY | EV_TX_SPACE | EV_LINK_CHANGE) == 0);
        if outcome.detached {
            assert!(self.tainted, "a clean ring is never poisoned");
        }
        if self.bed.engine.attached().is_none() {
            self.model.attached = None;
        }
        if !self.tainted {
            self.check_against_model(outcome);
        }
        Ok(())
    }

    /// Apply one pump to the model and compare.
    pub(super) fn check_against_model(&mut self, outcome: PumpOutcome) {
        let slots = self.client_slots();
        let m = &mut self.model;
        // Reap: transmits the device completed returned their slots, so the
        // free count is what is not still queued at the device.
        let mut free = self.tx_entries - m.dev_tx.len() as u16;
        // Receive.
        let mut delivered = 0u32;
        while let Some(frame) = m.dev_rx.pop_front() {
            let len = frame.len();
            if len < 14 {
                m.stats.rx_dropped += 1;
                m.stats.runts += 1;
            } else if len > POLICY_MAX {
                m.stats.rx_dropped += 1;
                m.stats.oversize += 1;
            } else {
                let pass = match m.mode {
                    0 => false,
                    2 => true,
                    _ => frame[0] & 1 != 0 || frame[..6] == MAC,
                };
                if !pass || m.attached.is_none() || m.client_rx.len() == slots {
                    m.stats.rx_dropped += 1;
                } else {
                    m.stats.rx_frames += 1;
                    m.stats.rx_bytes += len as u64;
                    m.client_rx.push_back(frame);
                    delivered += 1;
                }
            }
        }
        let mut events = 0;
        if delivered > 0 && m.rx_armed {
            events |= EV_RX_READY;
            m.rx_armed = false;
        }
        // Transmit.
        let mut sent = 0u32;
        if m.attached.is_some() {
            let was_full = m.client_tx.len() == slots;
            let mut consumed = 0;
            while free > 0 {
                let Some(frame) = m.client_tx.pop_front() else {
                    m.tx_armed = true;
                    break;
                };
                consumed += 1;
                let len = frame.len();
                if len < 14 {
                    m.stats.tx_dropped += 1;
                    m.stats.runts += 1;
                } else if len > POLICY_MAX {
                    m.stats.tx_dropped += 1;
                    m.stats.oversize += 1;
                } else {
                    m.stats.tx_frames += 1;
                    m.stats.tx_bytes += len as u64;
                    m.dev_tx.push_back(frame);
                    free -= 1;
                    sent += 1;
                }
            }
            if was_full && consumed > 0 {
                events |= EV_TX_SPACE;
            }
            if m.link_event {
                events |= EV_LINK_CHANGE;
            }
        }
        m.link_event = false;
        let s = self.bed.engine.stats();
        assert_eq!(outcome.rx_delivered, delivered);
        assert_eq!(outcome.tx_sent, sent);
        assert_eq!(outcome.events, events, "wake-up events");
        assert_eq!(
            (s.rx_frames, s.tx_frames, s.rx_bytes, s.tx_bytes),
            (
                m.stats.rx_frames,
                m.stats.tx_frames,
                m.stats.rx_bytes,
                m.stats.tx_bytes
            )
        );
        assert_eq!(
            (s.rx_dropped, s.tx_dropped, s.runts, s.oversize),
            (
                m.stats.rx_dropped,
                m.stats.tx_dropped,
                m.stats.runts,
                m.stats.oversize
            )
        );
        assert_eq!(s.ring_errors, 0, "a clean run has no ring errors");
        // Slot conservation: every receive buffer is with the device, every
        // transmit slot is free or queued at the device.
        assert_eq!(self.bed.engine.queues().rx_in_flight(), self.rx_entries);
        assert_eq!(self.bed.engine.queues().tx_free(), free);
    }

    pub(super) fn attach(&mut self, owner: u64, slots: u32) {
        let result = self.bed.attach(slots, owner);
        if self.tainted {
            return;
        }
        let expected = if self.model.attached.is_some() {
            Err(AttachError::Busy)
        } else if !framering::valid_slots(slots) {
            Err(AttachError::Invalid)
        } else {
            Ok(())
        };
        assert_eq!(result.map(|_| ()), expected);
        if let Ok(ring) = result {
            self.model.attached = Some((owner, ring));
            self.model.client_rx.clear();
            self.model.client_tx.clear();
            self.model.rx_armed = false;
            self.model.tx_armed = true;
        }
    }

    pub(super) fn detach(&mut self, owner: u64, ring: u32) {
        let result = self.bed.engine.detach(owner, ring);
        if self.tainted {
            if result.is_ok() {
                self.model.attached = None;
            }
            return;
        }
        let expected = match self.model.attached {
            None => Err(CtlError::NoRing),
            Some((o, _)) if o != owner => Err(CtlError::Denied),
            Some((_, r)) if r != ring => Err(CtlError::NoRing),
            Some(_) => Ok(()),
        };
        assert_eq!(result, expected);
        if result.is_ok() {
            self.model.attached = None;
        }
    }

    pub(super) fn scribble(&mut self, script: &mut Script) {
        let attached = self.bed.engine.attached().is_some();
        let Some(client) = self.bed.client.as_mut() else {
            return;
        };
        let one = ring_bytes(client.slots);
        let ring = usize::from(script.u8() & 1) * one;
        let kind = script.u8() % 6;
        let value = script.u32();
        let slot = usize::from(script.u16()) % client.slots as usize;
        let bytes = client.mem.bytes();
        let word = |bytes: &mut [u8], at: usize, v: u32| {
            bytes[at..at + 4].copy_from_slice(&v.to_le_bytes())
        };
        match kind {
            // The advisory flag only: delivery is unaffected, the model follows.
            0 => {
                word(bytes, ring + off::ARMED, value);
                if attached {
                    if ring == 0 {
                        self.model.rx_armed = value != 0;
                    } else {
                        self.model.tx_armed = value != 0;
                    }
                }
                return;
            }
            1 => word(bytes, ring + off::HEAD, value),
            2 => word(bytes, ring + off::TAIL, value),
            3 => word(
                bytes,
                ring + HEADER_BYTES + slot * SLOT_BYTES,
                value & 0xFFFF,
            ),
            4 => {
                let at = ring
                    + HEADER_BYTES
                    + slot * SLOT_BYTES
                    + 2
                    + (value as usize % (MAX_FRAME - 4));
                bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            _ => word(bytes, ring + off::MAGIC, value),
        }
        self.tainted = true;
    }

    pub(super) fn tx_frame_len(script: &mut Script) -> usize {
        match script.u8() % 6 {
            0 => 0,
            1 => 13,
            2 => 14,
            3 => 1514,
            4 => 1515,
            _ => usize::from(script.u16()) % 2100,
        }
    }

    pub(super) fn control(&mut self, op: u8, script: &mut Script) {
        let owner = OWNER + u64::from(script.u8() % 3);
        let ring = self.model.attached.map_or(1, |(_, r)| r) + u32::from(script.u8() % 2);
        match op {
            200..=204 => self.detach(owner, ring),
            205..=209 => {
                let mode = u32::from(script.u8() % 5);
                let result = self.bed.engine.set_rx_mode(owner, mode);
                if !self.tainted {
                    match self.model.attached {
                        None => assert_eq!(result, Err(CtlError::NoRing)),
                        Some((o, _)) if o != owner => assert_eq!(result, Err(CtlError::Denied)),
                        Some(_) if mode > 2 => assert_eq!(result, Ok(None)),
                        Some(_) => {
                            assert!(matches!(result, Ok(Some(_))));
                            self.model.mode = mode;
                        }
                    }
                }
            }
            _ => {
                let got = self.bed.engine.kick(owner, ring);
                if !self.tainted {
                    assert_eq!(got, self.model.attached == Some((owner, ring)));
                }
            }
        }
    }
}
