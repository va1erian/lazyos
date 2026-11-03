//! The controller model: state, the timeline, and the [`Controller`] trait.

use std::collections::VecDeque;

use bthci::acl::{self, Boundary};
use bthci::{BdAddr, Command, Event};
use btl2cap::frag::fragments;

use crate::lepeer::LePeer;
use crate::peer::BrPeer;
use crate::{Controller, HciIn};

/// Default event mask after Reset (Core Specification, Vol 4 Part E 7.3.1).
pub(crate) const DEFAULT_EVENT_MASK: u64 = 0x0000_1FFF_FFFF_FFFF;
pub(crate) const DEFAULT_LE_EVENT_MASK: u64 = 0x1F;
const MAX_LOG: usize = 4096;

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub bd_addr: BdAddr,
    /// ACL payload size and count the host may use (Read_Buffer_Size).
    pub acl_len: u16,
    pub acl_num: u16,
    /// Largest ACL fragment the controller sends to the host; small values
    /// make every L2CAP PDU arrive in pieces.
    pub rx_acl_len: usize,
    /// Answer the LE commands (false: "unknown command", a BR/EDR-only part).
    pub le_supported: bool,
    pub cmd_latency_ms: u64,
    pub reset_latency_ms: u64,
    /// Time a page takes to find a connectable peer.
    pub page_ms: u64,
    /// Seed for pairing keys and passkeys.
    pub seed: u64,
    /// A race the real transport has: an LE peer's first data after the
    /// encryption procedure reaches the host before the Encryption Change
    /// event (they travel on different USB endpoints).
    pub acl_beats_encryption: bool,
    /// Accept `Write_Inquiry_Mode(2)` (extended inquiry results carrying the
    /// name); false: names need a Remote_Name_Request.
    pub extended_inquiry: bool,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            bd_addr: BdAddr::from_msb([0x00, 0x1B, 0xDC, 0x0F, 0xEE, 0x01]),
            acl_len: 64,
            acl_num: 4,
            rx_acl_len: 48,
            le_supported: true,
            cmd_latency_ms: 1,
            reset_latency_ms: 5,
            page_ms: 20,
            seed: 0x5EED,
            acl_beats_encryption: false,
            extended_inquiry: true,
        }
    }
}

/// A broken rule of the host-controller interface, recorded for the test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Violation {
    /// A command arrived while the controller had granted no credit.
    CommandWithoutCredit {
        opcode: u16,
    },
    MalformedCommand,
    MalformedAcl,
    AclUnknownHandle {
        handle: u16,
    },
    /// More ACL packets outstanding than the controller has buffers.
    AclNoBuffer {
        handle: u16,
    },
    AclTooLong {
        handle: u16,
        len: usize,
    },
}

/// A simulated remote device.
#[non_exhaustive]
// A test model: the LE peer is large, there are a handful of them.
#[allow(clippy::large_enum_variant)]
pub enum Peer {
    Br(BrPeer),
    Le(LePeer),
}

impl Peer {
    /// The address the host knows it by (the identity address for LE).
    pub fn addr(&self) -> BdAddr {
        match self {
            Peer::Br(p) => p.addr,
            Peer::Le(p) => p.addr,
        }
    }
}

/// A pending `LE_Create_Connection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LeCreate {
    /// 0: connect to `peer`; 1: anything on the accept list.
    pub filter_policy: u8,
    pub peer_type: u8,
    pub peer: BdAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerId(pub usize);

pub(crate) use crate::task::Task;

struct Scheduled {
    due: u64,
    seq: u64,
    task: Task,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Auth {
    Idle,
    WaitLinkKey { by_peer: bool },
    WaitIoCap,
    WaitConfirm,
    WaitPasskey,
}

pub(crate) struct Conn {
    pub handle: u16,
    pub peer: usize,
    pub by_peer: bool,
    pub auth: Auth,
    /// Authentication succeeded on this link (encryption may be enabled).
    pub authenticated: bool,
    pub encrypted: bool,
    /// The passkey shown to the host during Passkey Entry.
    pub passkey: u32,
}

pub struct SimController {
    pub(crate) cfg: SimConfig,
    /// The time of the event being executed; scheduling is relative to it.
    pub(crate) clock: u64,
    seq: u64,
    timeline: Vec<Scheduled>,
    out: VecDeque<HciIn>,
    pub(crate) peers: Vec<Peer>,
    pub(crate) conns: Vec<Conn>,
    /// Peers that paged the host and wait for Accept/Reject.
    pub(crate) pending_requests: Vec<usize>,
    pub(crate) next_handle: u16,
    pub(crate) rng: u64,
    pub(crate) credits: u8,
    pub(crate) acl_in_use: u16,
    pub(crate) event_mask: u64,
    pub(crate) le_event_mask: u64,
    pub(crate) scan_enable: u8,
    pub(crate) ssp: bool,
    pub(crate) class_of_device: u32,
    pub(crate) local_name: Vec<u8>,
    pub(crate) accept_list: Vec<(u8, BdAddr)>,
    pub(crate) le_create: Option<LeCreate>,
    pub(crate) le_scanning: bool,
    pub(crate) inquiry_running: bool,
    pub(crate) inquiry_mode: bthci::inquiry::Mode,
    pub(crate) violations: Vec<Violation>,
    pub(crate) log: Vec<Command>,
}

impl SimController {
    pub fn new(cfg: SimConfig) -> SimController {
        SimController {
            rng: cfg.seed | 1,
            cfg,
            clock: 0,
            seq: 0,
            timeline: Vec::new(),
            out: VecDeque::new(),
            peers: Vec::new(),
            conns: Vec::new(),
            pending_requests: Vec::new(),
            next_handle: 0x0040,
            credits: 1,
            acl_in_use: 0,
            event_mask: DEFAULT_EVENT_MASK,
            le_event_mask: DEFAULT_LE_EVENT_MASK,
            scan_enable: 0,
            ssp: false,
            class_of_device: 0,
            local_name: Vec::new(),
            accept_list: Vec::new(),
            le_create: None,
            le_scanning: false,
            inquiry_running: false,
            inquiry_mode: bthci::inquiry::Mode::Standard,
            violations: Vec::new(),
            log: Vec::new(),
        }
    }

    pub fn add_br_peer(&mut self, peer: BrPeer) -> PeerId {
        self.peers.push(Peer::Br(peer));
        PeerId(self.peers.len() - 1)
    }

    pub fn add_le_peer(&mut self, peer: LePeer) -> PeerId {
        self.peers.push(Peer::Le(peer));
        PeerId(self.peers.len() - 1)
    }

    /// The BR/EDR peer `id` (panics if it is an LE peer: a test bug).
    pub fn br(&self, id: PeerId) -> &BrPeer {
        match &self.peers[id.0] {
            Peer::Br(p) => p,
            Peer::Le(_) => panic!("peer {} is an LE peer", id.0),
        }
    }

    /// The LE peer `id` (panics if it is a BR/EDR peer: a test bug).
    pub fn le(&self, id: PeerId) -> &LePeer {
        match &self.peers[id.0] {
            Peer::Le(p) => p,
            Peer::Br(_) => panic!("peer {} is a BR/EDR peer", id.0),
        }
    }

    pub fn le_mut(&mut self, id: PeerId) -> &mut LePeer {
        self.le_at(id.0)
    }

    pub(crate) fn le_at(&mut self, i: usize) -> &mut LePeer {
        match &mut self.peers[i] {
            Peer::Le(p) => p,
            Peer::Br(_) => panic!("peer {i} is a BR/EDR peer"),
        }
    }

    pub fn is_le(&self, id: PeerId) -> bool {
        matches!(self.peers[id.0], Peer::Le(_))
    }

    pub fn br_mut(&mut self, id: PeerId) -> &mut BrPeer {
        self.br_at(id.0)
    }

    pub(crate) fn br_at(&mut self, i: usize) -> &mut BrPeer {
        match &mut self.peers[i] {
            Peer::Br(p) => p,
            Peer::Le(_) => panic!("peer {i} is an LE peer"),
        }
    }

    pub(crate) fn peer_index(&self, addr: BdAddr) -> Option<usize> {
        self.peers
            .iter()
            .position(|p| matches!(p, Peer::Br(_)) && p.addr() == addr)
    }

    pub(crate) fn conn_index(&self, handle: u16) -> Option<usize> {
        self.conns.iter().position(|c| c.handle == handle)
    }

    pub(crate) fn conn_by_addr(&self, addr: BdAddr) -> Option<usize> {
        self.conns
            .iter()
            .position(|c| self.peers[c.peer].addr() == addr)
    }

    /// Schedule `task` `delay` ms after the current time.
    pub(crate) fn after(&mut self, delay: u64, task: Task) {
        self.seq += 1;
        let due = self.clock + delay;
        let at = self
            .timeline
            .partition_point(|s| (s.due, s.seq) <= (due, self.seq));
        self.timeline.insert(
            at,
            Scheduled {
                due,
                seq: self.seq,
                task,
            },
        );
    }

    pub(crate) fn discard_pending_output(&mut self) {
        self.out.clear();
    }

    pub(crate) fn drop_timeline(&mut self) {
        self.timeline.clear();
    }

    /// The next time something happens by itself (a scheduled event).
    pub fn next_due(&self) -> Option<u64> {
        self.timeline
            .first()
            .map(|s| s.due)
            .or_else(|| (!self.out.is_empty()).then_some(self.clock))
    }

    pub fn violations(&self) -> &[Violation] {
        &self.violations
    }

    /// Every command parsed so far, oldest first (capped).
    pub fn commands_seen(&self) -> &[Command] {
        &self.log
    }

    pub fn scan_enable(&self) -> u8 {
        self.scan_enable
    }

    pub fn ssp_enabled(&self) -> bool {
        self.ssp
    }

    pub fn event_mask(&self) -> u64 {
        self.event_mask
    }

    pub fn local_name(&self) -> &[u8] {
        &self.local_name
    }

    pub fn class_of_device(&self) -> u32 {
        self.class_of_device
    }

    pub fn accept_list(&self) -> &[(u8, BdAddr)] {
        &self.accept_list
    }

    pub fn le_connect_pending(&self) -> bool {
        self.le_create.is_some()
    }

    pub fn credits(&self) -> u8 {
        self.credits
    }

    /// Is the link to this peer encrypted on the controller's side?
    pub fn link_encrypted(&self, id: PeerId) -> bool {
        self.conns.iter().any(|c| c.peer == id.0 && c.encrypted)
    }

    pub fn link_handle(&self, id: PeerId) -> Option<u16> {
        self.conns.iter().find(|c| c.peer == id.0).map(|c| c.handle)
    }

    /// Drop the scheduled tasks `keep` refuses (a cancelled procedure).
    pub(crate) fn drop_tasks(&mut self, keep: impl Fn(&Task) -> bool) {
        self.timeline.retain(|s| keep(&s.task));
    }

    /// Run everything due by `now`, in order.
    pub(crate) fn run_until(&mut self, now: u64) {
        while self.timeline.first().is_some_and(|s| s.due <= now) {
            let s = self.timeline.remove(0);
            self.clock = self.clock.max(s.due);
            self.execute(s.task);
        }
        self.clock = self.clock.max(now);
    }

    /// Deliver an event, unless the host masked it out. Command Complete and
    /// Command Status are never masked and return the credit.
    pub(crate) fn emit(&mut self, ev: Event) {
        let code = ev.code();
        match &ev {
            Event::CommandComplete { num_commands, .. }
            | Event::CommandStatus { num_commands, .. } => {
                self.credits = *num_commands;
            }
            _ if code < 1 || self.event_mask >> (code - 1) & 1 == 0 => return,
            _ => {}
        }
        self.out.push_back(HciIn::Event(ev.encode()));
    }

    pub(crate) fn emit_acl(&mut self, packet: Vec<u8>) {
        self.out.push_back(HciIn::Acl(packet));
    }

    /// The peer sent these L2CAP PDUs: cut them into ACL packets for the host.
    pub(crate) fn peer_sends(&mut self, handle: u16, pdus: Vec<Vec<u8>>) {
        for pdu in pdus {
            for (boundary, piece) in fragments(&pdu, self.cfg.rx_acl_len) {
                if let Ok(packet) = acl::encode(handle, boundary, piece) {
                    self.after(1, Task::EmitAcl(packet));
                }
            }
        }
    }

    pub(crate) fn random(&mut self) -> u64 {
        // xorshift64*: deterministic and good enough for test keys.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub(crate) fn log_command(&mut self, cmd: &Command) {
        if self.log.len() < MAX_LOG {
            self.log.push(cmd.clone());
        }
    }
}

impl Controller for SimController {
    fn command(&mut self, now_ms: u64, packet: &[u8]) {
        self.run_until(now_ms);
        self.handle_packet(packet);
    }

    fn acl_out(&mut self, now_ms: u64, packet: &[u8]) {
        self.run_until(now_ms);
        let Ok((h, data)) = acl::parse(packet) else {
            self.violations.push(Violation::MalformedAcl);
            return;
        };
        let Some(ci) = self.conn_index(h.handle) else {
            self.violations
                .push(Violation::AclUnknownHandle { handle: h.handle });
            return;
        };
        if data.len() > usize::from(self.cfg.acl_len) {
            self.violations.push(Violation::AclTooLong {
                handle: h.handle,
                len: data.len(),
            });
            return;
        }
        if self.acl_in_use >= self.cfg.acl_num {
            self.violations
                .push(Violation::AclNoBuffer { handle: h.handle });
            return;
        }
        self.acl_in_use += 1;
        self.after(1, Task::AclDone { handle: h.handle });
        let peer = self.conns[ci].peer;
        if h.boundary != Boundary::Reserved {
            let now = self.clock;
            let pdus = match &mut self.peers[peer] {
                Peer::Br(p) => p.rx_acl(now, h.boundary, data),
                Peer::Le(p) => p.rx_acl(now, h.boundary, data),
            };
            self.peer_sends(h.handle, pdus);
        }
    }

    fn poll_in(&mut self, now_ms: u64) -> Option<HciIn> {
        self.run_until(now_ms);
        self.out.pop_front()
    }
}
