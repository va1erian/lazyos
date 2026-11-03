//! A scripted host for the end-to-end tests: a small stand-in for `btd`'s
//! logic, built from the same libraries (`bthci`, `btl2cap`, `btbond`) and
//! driven against [`btsim::SimController`] through the [`Controller`] trait.
//!
//! It is deliberately simple and strict: it respects command credits and ACL
//! buffers, admits HID channels only on a bonded, encrypted link, and keeps
//! its bonds in a [`BondStore`] that tests can persist and reload.

#![allow(dead_code)]

mod sdpclient;

use std::collections::VecDeque;

use btbond::{AddrKind, Bond, BondStore, Gatt, InputClass, LinkKey};
use bthci::acl::{self, Boundary};
use bthci::{AclBuffers, BdAddr, Command, CommandQueue, Event, Init, InitConfig, Input};
use btl2cap::frag::fragments;
use btl2cap::{Action, Config, Event as L2Event, L2cap, LinkKind};
use btsim::{Controller, HciIn, SimController};

pub const PSM_SDP: u16 = 1;
pub const PSM_CONTROL: u16 = 0x11;
pub const PSM_INTERRUPT: u16 = 0x13;
/// DisplayYesNo, general bonding with MITM protection.
const HOST_IO: u8 = 1;
const HOST_AUTH_REQ: u8 = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Note {
    ConnectionRequest(BdAddr),
    ConnectFailed(u8),
    AuthFailed(u8),
    PairingFailed(u8),
    Disconnected { handle: u16, reason: u8 },
    Refused(u16),
}

pub struct HostLink {
    pub handle: u16,
    pub addr: BdAddr,
    /// We created the link (paired or paged) rather than accepting it.
    pub initiated: bool,
    pub encrypted: bool,
    pub sdp: Option<u16>,
    pub control: Option<u16>,
    pub interrupt: Option<u16>,
    pub sdp_buf: Vec<u8>,
    pub tid: u16,
}

pub struct Host {
    pub sim: SimController,
    pub now: u64,
    pub init: Init,
    pub queue: CommandQueue,
    pub bufs: Option<AclBuffers>,
    pub l2: L2cap,
    pub store: BondStore,
    pub links: Vec<HostLink>,
    pub acl_tx: VecDeque<(u16, Boundary, Vec<u8>)>,
    pub notes: Vec<Note>,
    pub reports: Vec<Vec<u8>>,
    pub descriptor: Option<Vec<u8>>,
    pub shown_passkey: Option<u32>,
    pub jw_confirms: u32,
    pub sdp_requests: u32,
    pub armed_at: Option<u64>,
    pub first_request_ms: Option<u64>,
    pub started_at: u64,
    pub initiated: Vec<BdAddr>,
    pub events: Vec<Event>,
    /// Encrypt a link we created once it is authenticated (tests turn it off
    /// to see what the peer does with plain-link HID connections).
    pub auto_encrypt: bool,
}

impl Host {
    /// Start the host: init sequence begins at the current time.
    pub fn boot(sim: SimController, store: BondStore, now: u64) -> Host {
        let le: Vec<(u8, BdAddr)> = store
            .iter()
            .filter(|b| b.kind.is_le())
            .map(|b| (0, b.addr))
            .collect();
        let cfg = InitConfig {
            le_accept_list: le,
            ..InitConfig::default()
        };
        let mut host = Host {
            sim,
            now,
            init: Init::new(&cfg),
            queue: CommandQueue::new(),
            bufs: None,
            l2: L2cap::new(Config {
                psms: vec![PSM_CONTROL, PSM_INTERRUPT],
                ..Config::default()
            }),
            store,
            links: Vec::new(),
            acl_tx: VecDeque::new(),
            notes: Vec::new(),
            reports: Vec::new(),
            descriptor: None,
            shown_passkey: None,
            jw_confirms: 0,
            sdp_requests: 0,
            armed_at: None,
            first_request_ms: None,
            started_at: now,
            initiated: Vec::new(),
            events: Vec::new(),
            auto_encrypt: true,
        };
        let step = host.init.step(now, Input::Start);
        host.send(step.send);
        host
    }

    pub fn send(&mut self, cmd: Option<Command>) {
        if let Some(c) = cmd {
            self.queue.push(&c).expect("command queue has room");
        }
    }

    pub fn cmd(&mut self, c: Command) {
        self.send(Some(c));
    }

    /// Page `addr` and, once connected, authenticate and encrypt (pairing a
    /// device in pairing mode, or reconnecting a bonded one that waits).
    pub fn connect(&mut self, addr: BdAddr) {
        self.initiated.push(addr);
        self.cmd(Command::CreateConnection {
            addr,
            packet_type: 0xCC18,
            page_scan_repetition_mode: 2,
            clock_offset: 0,
            allow_role_switch: 1,
        });
    }

    /// Advance simulated time by `ms`, processing everything on the way.
    pub fn run_for(&mut self, ms: u64) {
        let end = self.now + ms;
        for _ in 0..1_000_000 {
            self.settle();
            match self.sim.next_due() {
                Some(t) if t <= end => self.now = self.now.max(t),
                _ => break,
            }
        }
        self.now = end;
        self.settle();
    }

    fn settle(&mut self) {
        loop {
            let mut progress = false;
            while let Some(packet) = self.queue.pop_ready(self.now) {
                self.sim.command(self.now, &packet);
                progress = true;
            }
            progress |= self.flush_acl();
            while let Some(incoming) = self.sim.poll_in(self.now) {
                self.on_hci(incoming);
                progress = true;
            }
            if !progress {
                return;
            }
        }
    }

    fn flush_acl(&mut self) -> bool {
        let mut sent = false;
        while let Some((handle, _, _)) = self.acl_tx.front() {
            let Some(bufs) = &mut self.bufs else { break };
            if bufs.acquire(*handle).is_err() {
                break;
            }
            let (handle, boundary, data) = self.acl_tx.pop_front().expect("front exists");
            let packet = acl::encode(handle, boundary, &data).expect("fits");
            self.sim.acl_out(self.now, &packet);
            sent = true;
        }
        sent
    }

    fn on_hci(&mut self, incoming: HciIn) {
        match incoming {
            HciIn::Event(bytes) => {
                let ev = Event::parse(&bytes).expect("the controller sends well-formed events");
                self.on_event(ev);
            }
            HciIn::Acl(bytes) => {
                let (h, data) = acl::parse(&bytes).expect("well-formed ACL");
                let actions = self.l2.rx_acl(self.now, h.handle, h.boundary, data);
                self.apply(actions);
            }
        }
    }

    fn link_mut(&mut self, handle: u16) -> Option<&mut HostLink> {
        self.links.iter_mut().find(|l| l.handle == handle)
    }

    fn link_by_addr(&self, addr: BdAddr) -> Option<&HostLink> {
        self.links.iter().find(|l| l.addr == addr)
    }

    fn on_event(&mut self, ev: Event) {
        self.events.push(ev.clone());
        self.queue.on_event(&ev);
        if let Some(b) = &mut self.bufs {
            b.on_completed(&ev);
        }
        match &ev {
            Event::CommandComplete { .. } | Event::CommandStatus { .. } => {
                self.on_command_result(&ev)
            }
            Event::ConnectionRequest { addr, .. } => {
                self.notes.push(Note::ConnectionRequest(*addr));
                self.first_request_ms.get_or_insert(self.now);
                let bonded = self.store.find(*addr, AddrKind::BrEdr).is_some();
                // A device we have no bond for is not admitted while pairing is off.
                self.cmd(if bonded {
                    Command::AcceptConnectionRequest {
                        addr: *addr,
                        role: 0,
                    }
                } else {
                    Command::RejectConnectionRequest {
                        addr: *addr,
                        reason: 0x0F,
                    }
                });
            }
            Event::ConnectionComplete {
                status: 0,
                handle,
                addr,
                ..
            } => self.on_connected(*handle, *addr),
            Event::ConnectionComplete { status, .. } => {
                self.notes.push(Note::ConnectFailed(*status))
            }
            Event::LinkKeyRequest { addr } => {
                let cmd = match self
                    .store
                    .find(*addr, AddrKind::BrEdr)
                    .and_then(|b| b.link_key.as_ref())
                {
                    Some(k) => Command::LinkKeyRequestReply {
                        addr: *addr,
                        key: k.key,
                    },
                    None => Command::LinkKeyRequestNegativeReply { addr: *addr },
                };
                self.cmd(cmd);
            }
            Event::IoCapabilityRequest { addr } => self.cmd(Command::IoCapabilityRequestReply {
                addr: *addr,
                io_capability: HOST_IO,
                oob_present: 0,
                authentication_requirements: HOST_AUTH_REQ,
            }),
            Event::UserConfirmationRequest { addr, .. } => {
                self.jw_confirms += 1;
                self.cmd(Command::UserConfirmationRequestReply { addr: *addr });
            }
            Event::UserPasskeyNotification { passkey, .. } => self.shown_passkey = Some(*passkey),
            Event::LinkKeyNotification {
                addr,
                key,
                key_type,
            } => self.remember(*addr, *key, *key_type),
            Event::SimplePairingComplete { status, .. } if *status != 0 => {
                self.notes.push(Note::PairingFailed(*status))
            }
            Event::AuthenticationComplete { status, handle } => {
                self.on_auth_complete(*handle, *status)
            }
            Event::EncryptionChange {
                status: 0,
                handle,
                enabled,
            } => self.on_encryption(*handle, *enabled),
            Event::DisconnectionComplete { handle, reason, .. } => {
                self.notes.push(Note::Disconnected {
                    handle: *handle,
                    reason: *reason,
                });
                self.links.retain(|l| l.handle != *handle);
                let actions = self.l2.link_down(*handle);
                self.apply(actions);
                if let Some(b) = &mut self.bufs {
                    b.link_closed(*handle);
                }
                self.acl_tx.retain(|(h, _, _)| h != handle);
            }
            _ => {}
        }
    }

    fn on_command_result(&mut self, ev: &Event) {
        let was_armed = self.init.is_armed();
        let step = self.init.step(self.now, Input::Event(ev));
        self.send(step.send);
        if !was_armed && self.init.is_armed() {
            self.armed_at = Some(self.now);
        }
        if self.bufs.is_none() && self.init.info().acl.acl_len != 0 {
            let b = self.init.info().acl;
            self.bufs = Some(AclBuffers::new(b.acl_len, b.acl_num));
        }
    }

    fn on_connected(&mut self, handle: u16, addr: BdAddr) {
        let initiated = self.initiated.contains(&addr);
        self.initiated.retain(|a| *a != addr);
        self.l2
            .link_up(handle, LinkKind::BrEdr)
            .expect("link table has room");
        self.links.push(HostLink {
            handle,
            addr,
            initiated,
            encrypted: false,
            sdp: None,
            control: None,
            interrupt: None,
            sdp_buf: Vec::new(),
            tid: 0,
        });
        if initiated {
            self.cmd(Command::AuthenticationRequested { handle });
        }
    }

    fn remember(&mut self, addr: BdAddr, key: [u8; 16], key_type: u8) {
        let authenticated = key_type == 5;
        let bond = Bond {
            addr,
            kind: AddrKind::BrEdr,
            authenticated,
            secure_connections: false,
            key_size: 16,
            link_key: Some(LinkKey { key, key_type }),
            ltk: None,
            irk: None,
            class_of_device: 0,
            // A keyboard needs an authenticated key; the class follows the bond.
            input: if authenticated {
                InputClass::Keyboard
            } else {
                InputClass::Pointer
            },
            hid_flags: 0,
            name: String::new(),
            report_descriptor: Vec::new(),
            gatt: Gatt::default(),
        };
        self.store.upsert(bond).expect("a valid bond");
    }

    fn on_auth_complete(&mut self, handle: u16, status: u8) {
        if status != 0 {
            self.notes.push(Note::AuthFailed(status));
        } else if self.auto_encrypt && self.link_mut(handle).is_some_and(|l| l.initiated) {
            self.cmd(Command::SetConnectionEncryption {
                handle,
                enable: true,
            });
        }
    }

    fn on_encryption(&mut self, handle: u16, enabled: bool) {
        let Some(link) = self.link_mut(handle) else {
            return;
        };
        link.encrypted = enabled;
        let (addr, initiated) = (link.addr, link.initiated);
        // Only a bonded device on an encrypted link may open HID channels.
        let bonded = self.store.find(addr, AddrKind::BrEdr).is_some();
        let allowed: &[u16] = if enabled && bonded {
            &[PSM_CONTROL, PSM_INTERRUPT]
        } else {
            &[]
        };
        self.l2.set_allowed(handle, allowed);
        if enabled && initiated {
            if let Ok((_, actions)) = self.l2.connect(self.now, handle, PSM_SDP) {
                self.apply(actions);
            }
        }
    }

    /// Carry out L2CAP actions: fragment sends for the controller's buffers.
    pub fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            match action {
                Action::Send { handle, pdu } => {
                    let max = self.bufs.as_ref().map_or(27, |b| usize::from(b.max_len()));
                    for (boundary, piece) in fragments(&pdu, max) {
                        self.acl_tx.push_back((handle, boundary, piece.to_vec()));
                    }
                }
                Action::Event(e) => self.on_l2_event(e),
            }
        }
    }

    fn on_l2_event(&mut self, e: L2Event) {
        match e {
            L2Event::Opened {
                handle,
                cid,
                psm,
                incoming,
                ..
            } => self.on_opened(handle, cid, psm, incoming),
            L2Event::Data { handle, cid, data } => self.on_data(handle, cid, data),
            L2Event::Closed { handle, cid, .. } => {
                if let Some(l) = self.link_mut(handle) {
                    for slot in [&mut l.sdp, &mut l.control, &mut l.interrupt] {
                        if *slot == Some(cid) {
                            *slot = None;
                        }
                    }
                }
            }
            L2Event::Refused { psm, .. } => self.notes.push(Note::Refused(psm)),
            L2Event::Misbehaved { handle } => self.cmd(Command::Disconnect {
                handle,
                reason: 0x13,
            }),
            L2Event::ConnectFailed { .. } => {}
        }
    }

    fn on_opened(&mut self, handle: u16, cid: u16, psm: u16, incoming: bool) {
        let Some(link) = self.link_mut(handle) else {
            return;
        };
        match psm {
            PSM_SDP => {
                link.sdp = Some(cid);
                self.sdp_request(handle, None);
            }
            PSM_CONTROL => {
                link.control = Some(cid);
                if !incoming {
                    if let Ok((_, actions)) = self.l2.connect(self.now, handle, PSM_INTERRUPT) {
                        self.apply(actions);
                    }
                }
            }
            PSM_INTERRUPT => link.interrupt = Some(cid),
            _ => {}
        }
    }

    fn on_data(&mut self, handle: u16, cid: u16, data: Vec<u8>) {
        let Some(link) = self.links.iter().find(|l| l.handle == handle) else {
            return;
        };
        if Some(cid) == link.sdp {
            self.on_sdp_response(handle, cid, &data);
        } else if Some(cid) == link.interrupt && data.first() == Some(&0xA1) {
            self.reports.push(data[1..].to_vec());
        }
    }

    pub fn send_on(&mut self, handle: u16, cid: u16, data: &[u8]) {
        let actions = self.l2.send(handle, cid, data).expect("channel is open");
        self.apply(actions);
    }

    pub fn link_for(&self, addr: BdAddr) -> Option<&HostLink> {
        self.link_by_addr(addr)
    }

    pub fn hid_ready(&self, addr: BdAddr) -> bool {
        self.link_by_addr(addr)
            .is_some_and(|l| l.control.is_some() && l.interrupt.is_some())
    }

    pub fn has_note(&self, n: &Note) -> bool {
        self.notes.contains(n)
    }
}
