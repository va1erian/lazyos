//! The channel manager: links, channels, and the calls the host makes.
//!
//! Pure state machine. Time is an argument; outputs are [`Action`]s the caller
//! carries out (send an L2CAP PDU as ACL fragments, tell the layer above).

use alloc::vec::Vec;
use bthci::Boundary;

use crate::link::{ChanState, Channel, Handle, Link, LinkKind};
use crate::pdu::{
    self, Command, Signal, CID_ATT, CID_CONNECTIONLESS, CID_DYNAMIC_MIN, CID_LE_SIGNALLING,
    CID_SIGNALLING, CID_SMP, DEFAULT_MTU,
};
use crate::Error;

/// Repeated protocol violations on one link before [`Event::Misbehaved`].
pub const MAX_VIOLATIONS: u8 = 16;

#[derive(Clone, Debug)]
pub struct Config {
    /// PSMs this host serves for incoming connections (HID: 0x11, 0x13).
    pub psms: Vec<u16>,
    /// The MTU announced for our end of every channel.
    pub mtu: u16,
    /// Bound on one reassembled PDU, header included.
    pub max_pdu: usize,
    pub max_links: usize,
    pub max_channels_per_link: usize,
    /// How long a signalling request may stay unanswered.
    pub rtx_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            psms: Vec::new(),
            mtu: DEFAULT_MTU,
            max_pdu: 4 + 1024,
            max_links: 8,
            max_channels_per_link: 8,
            rtx_ms: 5_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseReason {
    /// We asked.
    Local,
    /// The peer asked.
    Peer,
    /// A request went unanswered.
    Timeout,
    /// The peer refused our configuration.
    ConfigFailed,
    /// The peer sent a Command Reject for our request.
    Rejected,
    LinkDown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A dynamic channel finished configuration.
    Opened {
        handle: Handle,
        cid: u16,
        psm: u16,
        incoming: bool,
        peer_mtu: u16,
    },
    /// A payload on a dynamic channel or on LE fixed channel 4 (ATT) or 6 (SMP).
    Data {
        handle: Handle,
        cid: u16,
        data: Vec<u8>,
    },
    Closed {
        handle: Handle,
        cid: u16,
        reason: CloseReason,
    },
    /// Our outgoing connection did not happen (`result` 0xFFFF: timeout or
    /// Command Reject).
    ConnectFailed {
        handle: Handle,
        psm: u16,
        result: u16,
    },
    /// An incoming connection to a PSM we serve but the upper layer has not
    /// allowed on this link (not bonded, not encrypted): refused.
    Refused { handle: Handle, psm: u16 },
    /// Too many violations: the caller should disconnect the link.
    Misbehaved { handle: Handle },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// A complete L2CAP PDU for `handle`; cut it with [`crate::frag::fragments`].
    Send {
        handle: Handle,
        pdu: Vec<u8>,
    },
    Event(Event),
}

pub struct L2cap {
    pub(crate) cfg: Config,
    pub(crate) links: Vec<Link>,
}

impl L2cap {
    pub fn new(cfg: Config) -> L2cap {
        L2cap {
            cfg,
            links: Vec::new(),
        }
    }

    pub fn link_up(&mut self, handle: Handle, kind: LinkKind) -> Result<(), Error> {
        if self.links.iter().any(|l| l.handle == handle) || self.links.len() >= self.cfg.max_links {
            return Err(Error::Invalid);
        }
        self.links.push(Link::new(handle, kind, self.cfg.max_pdu));
        Ok(())
    }

    /// The link is gone; every channel on it closes.
    pub fn link_down(&mut self, handle: Handle) -> Vec<Action> {
        let Some(i) = self.links.iter().position(|l| l.handle == handle) else {
            return Vec::new();
        };
        let link = self.links.remove(i);
        link.channels
            .iter()
            .map(|c| closed(handle, c.local, CloseReason::LinkDown))
            .collect()
    }

    /// Set which PSMs may be opened by the peer on this link. Until the upper
    /// layer calls this (after bonding and encryption are confirmed) every
    /// incoming connection is refused.
    pub fn set_allowed(&mut self, handle: Handle, psms: &[u16]) -> bool {
        match self.link(handle) {
            Some(l) => {
                l.allowed = psms.to_vec();
                true
            }
            None => false,
        }
    }

    pub fn violations(&self, handle: Handle) -> u8 {
        self.links
            .iter()
            .find(|l| l.handle == handle)
            .map_or(0, |l| l.violations)
    }

    pub fn open_channels(&self, handle: Handle) -> usize {
        self.links
            .iter()
            .find(|l| l.handle == handle)
            .map_or(0, |l| {
                l.channels
                    .iter()
                    .filter(|c| c.state == ChanState::Open)
                    .count()
            })
    }

    /// `(psm, peer mtu)` of an open dynamic channel.
    pub fn channel_info(&self, handle: Handle, cid: u16) -> Option<(u16, u16)> {
        let link = self.links.iter().find(|l| l.handle == handle)?;
        let c = link
            .channels
            .iter()
            .find(|c| c.local == cid && c.state == ChanState::Open)?;
        Some((c.psm, c.peer_mtu))
    }

    pub(crate) fn link(&mut self, handle: Handle) -> Option<&mut Link> {
        self.links.iter_mut().find(|l| l.handle == handle)
    }

    /// Feed one ACL fragment from the controller.
    pub fn rx_acl(
        &mut self,
        now: u64,
        handle: Handle,
        boundary: Boundary,
        data: &[u8],
    ) -> Vec<Action> {
        let mut out = Vec::new();
        let L2cap { cfg, links } = self;
        let Some(link) = links.iter_mut().find(|l| l.handle == handle) else {
            return out;
        };
        match link.reasm.push(boundary, data) {
            Ok(Some(pdu)) => dispatch(cfg, link, now, &pdu, &mut out),
            Ok(None) => {}
            Err(_) => violation(link, &mut out),
        }
        out
    }

    /// Open a channel to `psm` on a BR/EDR link. Returns our channel id.
    pub fn connect(
        &mut self,
        now: u64,
        handle: Handle,
        psm: u16,
    ) -> Result<(u16, Vec<Action>), Error> {
        let (rtx, mtu, max) = (
            self.cfg.rtx_ms,
            self.cfg.mtu,
            self.cfg.max_channels_per_link,
        );
        let link = self.link(handle).ok_or(Error::Invalid)?;
        if link.kind != LinkKind::BrEdr || link.channels.len() >= max {
            return Err(Error::Invalid);
        }
        let cid = link.free_cid().ok_or(Error::Invalid)?;
        let mut ch = Channel::new(cid, psm, false, mtu, now, rtx);
        ch.ident = link.ident();
        let mut out = Vec::new();
        send_signal(
            link,
            ch.ident,
            Signal::ConnectionRequest { psm, scid: cid },
            &mut out,
        );
        link.channels.push(ch);
        Ok((cid, out))
    }

    /// Close a channel.
    pub fn disconnect(&mut self, now: u64, handle: Handle, cid: u16) -> Result<Vec<Action>, Error> {
        let rtx = self.cfg.rtx_ms;
        let link = self.link(handle).ok_or(Error::Invalid)?;
        let ident = link.ident();
        let ch = link.by_local(cid).ok_or(Error::Invalid)?;
        let mut out = Vec::new();
        if ch.state == ChanState::WaitConnectRsp {
            link.remove(cid);
            out.push(closed(handle, cid, CloseReason::Local));
            return Ok(out);
        }
        ch.state = ChanState::WaitDisconnectRsp;
        ch.ident = ident;
        ch.deadline = now.saturating_add(rtx);
        let sig = Signal::DisconnectRequest {
            dcid: ch.remote,
            scid: cid,
        };
        send_signal(link, ident, sig, &mut out);
        Ok(out)
    }

    /// Send a payload on an open channel (or LE ATT / SMP).
    pub fn send(&mut self, handle: Handle, cid: u16, data: &[u8]) -> Result<Vec<Action>, Error> {
        let link = self.link(handle).ok_or(Error::Invalid)?;
        let fixed_le = link.kind == LinkKind::Le && (cid == CID_ATT || cid == CID_SMP);
        if !fixed_le {
            let ch = link.by_local(cid).ok_or(Error::Invalid)?;
            if ch.state != ChanState::Open {
                return Err(Error::Invalid);
            }
            if data.len() > usize::from(ch.peer_mtu) {
                return Err(Error::TooLong);
            }
        }
        let pdu = pdu::encode_basic(self.wire_cid(handle, cid), data)?;
        Ok(alloc::vec![Action::Send { handle, pdu }])
    }

    /// The channel id to put on the wire: the *peer's* id for a dynamic
    /// channel, the fixed id as it is.
    fn wire_cid(&mut self, handle: Handle, cid: u16) -> u16 {
        match self.link(handle).and_then(|l| l.by_local(cid)) {
            Some(c) => c.remote,
            None => cid,
        }
    }

    /// Expire requests that went unanswered.
    pub fn tick(&mut self, now: u64) -> Vec<Action> {
        let mut out = Vec::new();
        for link in &mut self.links {
            let expired: Vec<u16> = link
                .channels
                .iter()
                .filter(|c| c.deadline <= now)
                .map(|c| c.local)
                .collect();
            for cid in expired {
                expire(link, cid, &mut out);
            }
        }
        out
    }

    /// When [`L2cap::tick`] next has something to do.
    pub fn next_deadline(&self) -> Option<u64> {
        self.links
            .iter()
            .flat_map(|l| l.channels.iter().map(|c| c.deadline))
            .filter(|d| *d != u64::MAX)
            .min()
    }
}

pub(crate) fn closed(handle: Handle, cid: u16, reason: CloseReason) -> Action {
    Action::Event(Event::Closed {
        handle,
        cid,
        reason,
    })
}

fn expire(link: &mut Link, cid: u16, out: &mut Vec<Action>) {
    let Some(ch) = link.by_local(cid) else {
        return;
    };
    let (state, psm, remote, incoming) = (ch.state, ch.psm, ch.remote, ch.incoming);
    link.remove(cid);
    if state == ChanState::WaitConnectRsp && !incoming {
        out.push(Action::Event(Event::ConnectFailed {
            handle: link.handle,
            psm,
            result: 0xFFFF,
        }));
        return;
    }
    out.push(closed(link.handle, cid, CloseReason::Timeout));
    if state != ChanState::WaitDisconnectRsp {
        let ident = link.ident();
        send_signal(
            link,
            ident,
            Signal::DisconnectRequest {
                dcid: remote,
                scid: cid,
            },
            out,
        );
    }
}

/// Queue a signalling command on the link's signalling channel.
pub(crate) fn send_signal(link: &Link, ident: u8, signal: Signal, out: &mut Vec<Action>) {
    let cid = match link.kind {
        LinkKind::BrEdr => CID_SIGNALLING,
        LinkKind::Le => CID_LE_SIGNALLING,
    };
    let body = Command { ident, signal }.encode();
    if let Ok(pdu) = pdu::encode_basic(cid, &body) {
        out.push(Action::Send {
            handle: link.handle,
            pdu,
        });
    }
}

/// Count a protocol violation; after [`MAX_VIOLATIONS`] tell the caller once.
pub(crate) fn violation(link: &mut Link, out: &mut Vec<Action>) {
    link.violations = link.violations.saturating_add(1);
    if link.violations >= MAX_VIOLATIONS && !link.misbehaved_reported {
        link.misbehaved_reported = true;
        out.push(Action::Event(Event::Misbehaved {
            handle: link.handle,
        }));
    }
}

fn dispatch(cfg: &Config, link: &mut Link, now: u64, pdu: &[u8], out: &mut Vec<Action>) {
    let Ok((cid, payload)) = pdu::parse_basic(pdu) else {
        return violation(link, out);
    };
    let signalling = match link.kind {
        LinkKind::BrEdr => CID_SIGNALLING,
        LinkKind::Le => CID_LE_SIGNALLING,
    };
    if cid == signalling {
        return crate::signal::on_payload(cfg, link, now, payload, out);
    }
    match (link.kind, cid) {
        (LinkKind::Le, CID_ATT | CID_SMP) => {
            out.push(Action::Event(Event::Data {
                handle: link.handle,
                cid,
                data: payload.to_vec(),
            }));
        }
        (LinkKind::BrEdr, CID_CONNECTIONLESS) => {}
        (_, c) if c >= CID_DYNAMIC_MIN && link.kind == LinkKind::BrEdr => {
            dynamic_data(link, cid, payload, out)
        }
        _ => violation(link, out),
    }
}

fn dynamic_data(link: &mut Link, cid: u16, payload: &[u8], out: &mut Vec<Action>) {
    let handle = link.handle;
    match link.by_local(cid) {
        Some(c) if c.state == ChanState::Open && payload.len() <= usize::from(c.local_mtu) => {
            out.push(Action::Event(Event::Data {
                handle,
                cid,
                data: payload.to_vec(),
            }));
        }
        _ => violation(link, out),
    }
}
