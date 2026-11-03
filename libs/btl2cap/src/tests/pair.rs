//! Two managers wired back to back over a link with a tiny ACL size, so every
//! PDU is fragmented and reassembled on the way.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use bthci::Boundary;

use crate::frag::fragments;
use crate::manager::{Action, Config, Event, L2cap};
use crate::pdu::{self, Command, Signal, CID_SIGNALLING};
use crate::LinkKind;

pub const H: u16 = 0x40;

pub struct Side {
    pub l2: L2cap,
    pub events: Vec<Event>,
}

pub struct Pair {
    pub a: Side,
    pub b: Side,
    pub now: u64,
    pub acl_len: usize,
    /// PDUs a side sent, for assertions on what went over the air.
    pub wire: Vec<(bool, Vec<u8>)>,
}

fn side(psms: &[u16], kind: LinkKind) -> Side {
    let mut l2 = L2cap::new(Config {
        psms: psms.to_vec(),
        ..Config::default()
    });
    l2.link_up(H, kind).unwrap();
    Side {
        l2,
        events: Vec::new(),
    }
}

impl Pair {
    /// `a` connects out; `b` serves 0x11 and 0x13 and (when `allow`) admits them.
    pub fn new(allow: bool) -> Pair {
        let mut p = Pair {
            a: side(&[], LinkKind::BrEdr),
            b: side(&[0x11, 0x13], LinkKind::BrEdr),
            now: 0,
            acl_len: 7,
            wire: Vec::new(),
        };
        if allow {
            p.b.l2.set_allowed(H, &[0x11, 0x13]);
        }
        p
    }

    pub fn le() -> Pair {
        Pair {
            a: side(&[], LinkKind::Le),
            b: side(&[], LinkKind::Le),
            now: 0,
            acl_len: 7,
            wire: Vec::new(),
        }
    }

    /// Carry `actions` produced by side `from_a`, and everything they cause.
    pub fn run(&mut self, from_a: bool, actions: Vec<Action>) {
        let mut queue: VecDeque<(bool, Action)> =
            actions.into_iter().map(|a| (from_a, a)).collect();
        while let Some((from_a, action)) = queue.pop_front() {
            match action {
                Action::Event(e) => {
                    if from_a { &mut self.a } else { &mut self.b }
                        .events
                        .push(e);
                }
                Action::Send { handle, pdu } => {
                    self.wire.push((from_a, pdu.clone()));
                    let (acl_len, now) = (self.acl_len, self.now);
                    let to = if from_a { &mut self.b } else { &mut self.a };
                    for (boundary, piece) in fragments(&pdu, acl_len) {
                        let more = to.l2.rx_acl(now, handle, boundary, piece);
                        queue.extend(more.into_iter().map(|m| (!from_a, m)));
                    }
                }
            }
        }
    }

    pub fn open(&mut self, psm: u16) -> (u16, u16) {
        let (cid_a, acts) = self.a.l2.connect(self.now, H, psm).unwrap();
        self.run(true, acts);
        let cid_b = self
            .b
            .events
            .iter()
            .find_map(|e| match e {
                Event::Opened { cid, psm: p, .. } if *p == psm => Some(*cid),
                _ => None,
            })
            .expect("b opened");
        (cid_a, cid_b)
    }
}

/// A signalling PDU carrying one command.
pub fn sig_pdu(ident: u8, signal: Signal) -> Vec<u8> {
    pdu::encode_basic(CID_SIGNALLING, &Command { ident, signal }.encode()).unwrap()
}

pub fn inject(p: &mut Pair, to_b: bool, bytes: &[u8]) -> Vec<Action> {
    let side = if to_b { &mut p.b } else { &mut p.a };
    side.l2.rx_acl(p.now, H, Boundary::FirstNonFlushable, bytes)
}
