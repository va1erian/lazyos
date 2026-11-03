//! Per-link and per-channel state.

use alloc::vec::Vec;

use crate::frag::Reassembler;
use crate::pdu::{CID_DYNAMIC_MIN, DEFAULT_MTU};

/// An HCI connection handle.
pub type Handle = u16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    BrEdr,
    Le,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChanState {
    /// We sent a connection request.
    WaitConnectRsp,
    /// Connected; configuring in both directions.
    Config,
    Open,
    /// We sent a disconnection request.
    WaitDisconnectRsp,
}

#[derive(Debug)]
pub struct Channel {
    pub local: u16,
    pub remote: u16,
    pub psm: u16,
    pub state: ChanState,
    pub incoming: bool,
    /// The largest payload we accept on this channel.
    pub local_mtu: u16,
    /// The largest payload the peer accepts.
    pub peer_mtu: u16,
    /// The peer accepted our configuration request.
    pub local_done: bool,
    /// We accepted the peer's configuration request.
    pub remote_done: bool,
    /// Identifier of our outstanding request, 0 when none.
    pub ident: u8,
    pub deadline: u64,
}

impl Channel {
    pub fn new(local: u16, psm: u16, incoming: bool, local_mtu: u16, now: u64, rtx: u64) -> Self {
        Channel {
            local,
            remote: 0,
            psm,
            state: ChanState::WaitConnectRsp,
            incoming,
            local_mtu,
            peer_mtu: DEFAULT_MTU,
            local_done: false,
            remote_done: false,
            ident: 0,
            deadline: now.saturating_add(rtx),
        }
    }
}

#[derive(Debug)]
pub struct Link {
    pub handle: Handle,
    pub kind: LinkKind,
    pub reasm: Reassembler,
    pub channels: Vec<Channel>,
    /// PSMs the upper layer admits on this link right now.
    pub allowed: Vec<u16>,
    pub next_ident: u8,
    pub violations: u8,
    pub misbehaved_reported: bool,
}

impl Link {
    pub fn new(handle: Handle, kind: LinkKind, max_pdu: usize) -> Link {
        Link {
            handle,
            kind,
            reasm: Reassembler::new(max_pdu),
            channels: Vec::new(),
            allowed: Vec::new(),
            next_ident: 0,
            violations: 0,
            misbehaved_reported: false,
        }
    }

    /// The next request identifier, never 0.
    pub fn ident(&mut self) -> u8 {
        self.next_ident = self.next_ident % 255 + 1;
        self.next_ident
    }

    pub fn by_local(&mut self, cid: u16) -> Option<&mut Channel> {
        self.channels.iter_mut().find(|c| c.local == cid)
    }

    pub fn remote_in_use(&self, remote: u16) -> bool {
        self.channels.iter().any(|c| c.remote == remote)
    }

    /// The lowest free dynamic channel id.
    pub fn free_cid(&self) -> Option<u16> {
        (CID_DYNAMIC_MIN..CID_DYNAMIC_MIN + 64)
            .find(|cid| self.channels.iter().all(|c| c.local != *cid))
    }

    pub fn remove(&mut self, cid: u16) {
        self.channels.retain(|c| c.local != cid);
    }
}
