//! A simulated BR/EDR HID peripheral: its own L2CAP, an SDP server and HIDP.
//!
//! The peer knows nothing of HCI. The controller model ([`crate::SimController`])
//! carries ACL data between the host and [`BrPeer::rx_acl`], and runs the link
//! manager (authentication, pairing, encryption) using the properties the peer
//! declares here: its IO capability, whether it is in pairing mode, and the
//! link key it holds.

use std::collections::VecDeque;
use std::string::String;
use std::vec::Vec;

use bthci::{BdAddr, Boundary};
use btl2cap::{Action, Config, Event, Handle, L2cap, LinkKind};

use crate::hid::{self, HidKind};
use crate::sdp;

/// Reports queued while the interrupt channel is down.
const MAX_PENDING_REPORTS: usize = 16;
const PSM_HID_CONTROL: u16 = 0x11;
const PSM_HID_INTERRUPT: u16 = 0x13;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasskeyBehavior {
    /// The user types the displayed passkey correctly.
    Correct,
    /// The user types it wrong.
    Wrong,
    /// Nobody types it (the host must time out).
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectStyle {
    /// The peer pages the host when the user touches it (HIDReconnectInitiate).
    PeerInitiates,
    /// The peer waits to be paged by the host (HIDNormallyConnectable).
    HostMustPage,
}

pub(crate) struct PeerConn {
    pub handle: Handle,
    pub encrypted: bool,
    pub peer_initiated: bool,
    l2: L2cap,
    sdp: Option<u16>,
    control: Option<u16>,
    interrupt: Option<u16>,
}

pub struct BrPeer {
    pub addr: BdAddr,
    pub name: String,
    pub kind: HidKind,
    /// HCI IO capability: 2 KeyboardOnly, 3 NoInputNoOutput, ...
    pub io_capability: u8,
    pub passkey: PasskeyBehavior,
    pub reconnect: ReconnectStyle,
    /// Bytes per SDP response; small values force continuation.
    pub sdp_chunk: usize,
    /// Accepting new bonds (the user held the pairing button).
    pub pairing_mode: bool,
    /// The bond: link key and its HCI key type.
    pub link_key: Option<([u8; 16], u8)>,
    pub(crate) conn: Option<PeerConn>,
    pub(crate) pending: VecDeque<Vec<u8>>,
    /// Page the host until this time (a reconnect in progress).
    pub(crate) paging_until: Option<u64>,
    /// Boot protocol selected with SET_PROTOCOL.
    pub boot_protocol: bool,
    /// Output reports the host sent (keyboard LEDs).
    pub output_reports: Vec<Vec<u8>>,
    /// HID_CONTROL virtual cable unplug received.
    pub unplugged: bool,
    pub sdp_requests: u32,
    /// Incoming L2CAP connections the peer's own policy refused.
    pub refused: u32,
    /// Open the HID channels as soon as the link is encrypted, even when the
    /// host paged it (and so while the host is still running SDP).
    pub eager_channels: bool,
}

impl BrPeer {
    fn new(addr: [u8; 6], kind: HidKind, name: &str, io_capability: u8) -> BrPeer {
        BrPeer {
            addr: BdAddr::from_msb(addr),
            name: name.into(),
            kind,
            io_capability,
            passkey: PasskeyBehavior::Correct,
            reconnect: ReconnectStyle::PeerInitiates,
            sdp_chunk: 512,
            pairing_mode: false,
            link_key: None,
            conn: None,
            pending: VecDeque::new(),
            paging_until: None,
            boot_protocol: false,
            output_reports: Vec::new(),
            unplugged: false,
            sdp_requests: 0,
            refused: 0,
            eager_channels: false,
        }
    }

    /// A keyboard: KeyboardOnly IO, so pairing uses Passkey Entry.
    pub fn keyboard(addr_msb: [u8; 6]) -> BrPeer {
        BrPeer::new(addr_msb, HidKind::Keyboard, "Sim Keyboard", 2)
    }

    /// A mouse: NoInputNoOutput, so pairing is Just Works.
    pub fn mouse(addr_msb: [u8; 6]) -> BrPeer {
        BrPeer::new(addr_msb, HidKind::Mouse, "Sim Mouse", 3)
    }

    /// Enter pairing mode. Like most devices, this drops the old bond.
    pub fn set_pairing_mode(&mut self, on: bool) {
        self.pairing_mode = on;
        if on {
            self.link_key = None;
        }
    }

    pub fn is_connected(&self) -> bool {
        self.conn.is_some()
    }

    pub fn is_encrypted(&self) -> bool {
        self.conn.as_ref().is_some_and(|c| c.encrypted)
    }

    pub fn handle(&self) -> Option<Handle> {
        self.conn.as_ref().map(|c| c.handle)
    }

    /// `(control, interrupt)` channels are open.
    pub fn hid_channels_open(&self) -> (bool, bool) {
        self.conn.as_ref().map_or((false, false), |c| {
            (c.control.is_some(), c.interrupt.is_some())
        })
    }

    /// Would a page from the host find this device?
    pub fn connectable(&self) -> bool {
        self.conn.is_none()
            && (self.pairing_mode
                || (self.link_key.is_some() && self.reconnect == ReconnectStyle::HostMustPage))
    }

    pub(crate) fn link_up(&mut self, handle: Handle, peer_initiated: bool) {
        let mut l2 = L2cap::new(Config {
            psms: vec![sdp::PSM_SDP, PSM_HID_CONTROL, PSM_HID_INTERRUPT],
            ..Config::default()
        });
        l2.link_up(handle, LinkKind::BrEdr)
            .expect("a fresh L2CAP has room for one link");
        l2.set_allowed(handle, &[sdp::PSM_SDP]);
        self.conn = Some(PeerConn {
            handle,
            encrypted: false,
            peer_initiated,
            l2,
            sdp: None,
            control: None,
            interrupt: None,
        });
        self.paging_until = None;
    }

    pub(crate) fn link_down(&mut self) {
        self.conn = None;
    }

    /// Encryption changed: the HID channels are admitted only while encrypted
    /// (the peer, like a good stack, refuses them on a plain link).
    pub(crate) fn set_encrypted(&mut self, on: bool) {
        if let Some(c) = &mut self.conn {
            c.encrypted = on;
            let psms: &[u16] = if on {
                &[sdp::PSM_SDP, PSM_HID_CONTROL, PSM_HID_INTERRUPT]
            } else {
                &[sdp::PSM_SDP]
            };
            c.l2.set_allowed(c.handle, psms);
        }
    }

    /// The L2CAP PDUs to send after a link the peer opened is encrypted.
    pub(crate) fn start_channels(&mut self, now: u64) -> Vec<Vec<u8>> {
        let eager = self.eager_channels;
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        if !c.peer_initiated && !eager {
            return Vec::new();
        }
        let Ok((_, actions)) = c.l2.connect(now, c.handle, PSM_HID_CONTROL) else {
            return Vec::new();
        };
        self.pump(now, actions)
    }

    /// One ACL fragment from the host; returns PDUs to send back.
    pub(crate) fn rx_acl(&mut self, now: u64, boundary: Boundary, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let actions = c.l2.rx_acl(now, c.handle, boundary, data);
        self.pump(now, actions)
    }

    /// The L2CAP PDUs that carry `message` on the control channel (empty
    /// while it is not open).
    pub(crate) fn control_pdus(&mut self, message: &[u8]) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let Some(cid) = c.control else {
            return Vec::new();
        };
        let actions = c.l2.send(c.handle, cid, message).unwrap_or_default();
        actions
            .into_iter()
            .filter_map(|a| match a {
                Action::Send { pdu, .. } => Some(pdu),
                Action::Event(_) => None,
            })
            .collect()
    }

    /// Queue an input report (the bytes after the `0xA1` header).
    pub fn queue_report(&mut self, payload: Vec<u8>) {
        if self.pending.len() < MAX_PENDING_REPORTS {
            self.pending.push_back(payload);
        }
    }

    /// Send queued reports if the interrupt channel is up.
    pub(crate) fn flush_reports(&mut self) -> Vec<Vec<u8>> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        let Some(cid) = c.interrupt else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Some(report) = self.pending.pop_front() {
            let mut data = vec![hid::hidp::DATA_INPUT];
            data.extend_from_slice(&report);
            for a in c.l2.send(c.handle, cid, &data).unwrap_or_default() {
                if let Action::Send { pdu, .. } = a {
                    out.push(pdu);
                }
            }
        }
        out
    }

    /// Run the actions through the upper layers until nothing more happens.
    fn pump(&mut self, now: u64, actions: Vec<Action>) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let mut work: VecDeque<Action> = actions.into();
        while let Some(action) = work.pop_front() {
            match action {
                Action::Send { pdu, .. } => out.push(pdu),
                Action::Event(e) => {
                    let more = self.react(now, e);
                    work.extend(more);
                }
            }
        }
        out.extend(self.flush_reports());
        out
    }

    fn react(&mut self, now: u64, event: Event) -> Vec<Action> {
        let Some((h, sdp_cid, control, interrupt)) = self
            .conn
            .as_ref()
            .map(|c| (c.handle, c.sdp, c.control, c.interrupt))
        else {
            return Vec::new();
        };
        let reply_on = |peer: &mut BrPeer, cid: u16, bytes: &[u8]| {
            let c = peer.conn.as_mut()?;
            c.l2.send(h, cid, bytes).ok()
        };
        match event {
            Event::Opened {
                cid, psm, incoming, ..
            } => {
                let Some(c) = self.conn.as_mut() else {
                    return Vec::new();
                };
                match psm {
                    sdp::PSM_SDP => c.sdp = Some(cid),
                    PSM_HID_CONTROL => {
                        c.control = Some(cid);
                        if !incoming {
                            return c
                                .l2
                                .connect(now, h, PSM_HID_INTERRUPT)
                                .map(|(_, a)| a)
                                .unwrap_or_default();
                        }
                    }
                    PSM_HID_INTERRUPT => c.interrupt = Some(cid),
                    _ => {}
                }
            }
            Event::Refused { .. } => self.refused += 1,
            Event::Closed { cid, .. } => {
                if let Some(c) = self.conn.as_mut() {
                    for slot in [&mut c.sdp, &mut c.control, &mut c.interrupt] {
                        if *slot == Some(cid) {
                            *slot = None;
                        }
                    }
                }
            }
            Event::Data { cid, data, .. } if Some(cid) == sdp_cid => {
                self.sdp_requests += 1;
                let record = sdp::hid_record(
                    self.kind,
                    self.reconnect == ReconnectStyle::PeerInitiates,
                    self.reconnect == ReconnectStyle::HostMustPage,
                    &self.name,
                );
                let reply = sdp::respond(&data, &record, self.sdp_chunk);
                return reply_on(self, cid, &reply).unwrap_or_default();
            }
            Event::Data { cid, data, .. } if Some(cid) == control => {
                let reply = self.hidp_control(&data);
                return reply_on(self, cid, &reply).unwrap_or_default();
            }
            Event::Data { cid, data, .. }
                if Some(cid) == interrupt && data.first() == Some(&hid::hidp::DATA_OUTPUT) =>
            {
                self.output_reports.push(data[1..].to_vec());
            }
            _ => {}
        }
        Vec::new()
    }

    /// Handle a HIDP message on the control channel; returns the reply.
    fn hidp_control(&mut self, msg: &[u8]) -> Vec<u8> {
        use hid::hidp::*;
        let Some(&head) = msg.first() else {
            return vec![HANDSHAKE_UNSUPPORTED];
        };
        match head & 0xF0 {
            SET_PROTOCOL => {
                self.boot_protocol = head & 1 == 0;
                vec![HANDSHAKE_SUCCESS]
            }
            SET_REPORT => {
                self.output_reports.push(msg[1..].to_vec());
                vec![HANDSHAKE_SUCCESS]
            }
            HID_CONTROL if head & 0x0F == VIRTUAL_CABLE_UNPLUG => {
                self.unplugged = true;
                self.link_key = None;
                vec![HANDSHAKE_SUCCESS]
            }
            _ => vec![HANDSHAKE_UNSUPPORTED],
        }
    }
}
