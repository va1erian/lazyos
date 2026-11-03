//! The host: state, the entry points the I/O shell calls, and the controller
//! bring-up glue. The pairing, channel and paging logic live in `conn.rs`,
//! `chan.rs`, `pair.rs` and `paging.rs`.
//!
//! The shell's loop is: feed what arrived (`on_event`, `on_acl`), call
//! `on_tick` when the deadline passes, then drain `poll_command`, `poll_acl`
//! and `poll_effect`, and sleep until `next_deadline` or the next doorbell.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;

use btbond::{AddrKind, BondStore};
use bthci::acl::{self, Boundary};
use bthci::flow::COMMAND_TIMEOUT_MS;
use bthci::opcode::status;
use bthci::{
    AclBuffers, BdAddr, Command, CommandQueue, ControllerInfo, Event, Init, InitConfig, Input,
    Phase,
};
use btl2cap::frag::fragments;
use btl2cap::{Config as L2Config, L2cap};

use crate::config::Config;
use crate::effect::Effect;
use crate::link::{Link, MAX_LINKS};
use crate::paging::Pager;

/// ACL fragments waiting for a controller buffer, over all links.
pub const MAX_ACL_QUEUE: usize = 512;
/// The ACL payload size assumed before `Read_Buffer_Size` has answered (the
/// smallest an LE-capable controller may report).
const DEFAULT_ACL_LEN: usize = 27;

/// Counters for what the host refused or dropped, for tests and `dbgd`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Packets that did not parse.
    pub malformed: u32,
    /// Connections and channels refused by policy.
    pub refused: u32,
    /// Reports dropped because the link was not a registered device.
    pub dropped_reports: u32,
    /// Commands or ACL fragments dropped because a queue was full.
    pub overflow: u32,
}

pub struct Host {
    pub(crate) cfg: Config,
    pub(crate) store: BondStore,
    pub(crate) init: Init,
    pub(crate) queue: CommandQueue,
    pub(crate) bufs: Option<AclBuffers>,
    pub(crate) l2: L2cap,
    pub(crate) links: Vec<Link>,
    pub(crate) acl_tx: VecDeque<(u16, Boundary, Vec<u8>)>,
    pub(crate) effects: VecDeque<Effect>,
    pub(crate) last_cmd_ms: u64,
    pub(crate) init_deadline: Option<u64>,
    pub(crate) armed_at: Option<u64>,
    pub(crate) window_until: Option<u64>,
    pub(crate) pager: Pager,
    pub(crate) stats: Stats,
    ctrl_reported: bool,
    failed: bool,
}

impl Host {
    /// Start the host at `now` (milliseconds on the shell's clock) with the
    /// bonds read from disk. The first command (`HCI_Reset`) is queued.
    pub fn new(cfg: Config, store: BondStore, now: u64) -> Host {
        let accept_list: Vec<(u8, BdAddr)> = store
            .iter()
            .filter(|b| b.kind.is_le())
            .map(|b| (u8::from(b.kind == AddrKind::LeRandom), b.addr))
            .collect();
        let init = Init::new(&InitConfig {
            le_accept_list: accept_list,
            ..InitConfig::default()
        });
        let l2 = L2cap::new(L2Config {
            psms: alloc::vec![bthid::PSM_CONTROL, bthid::PSM_INTERRUPT],
            max_links: MAX_LINKS,
            ..L2Config::default()
        });
        let mut host = Host {
            cfg,
            store,
            init,
            queue: CommandQueue::new(),
            bufs: None,
            l2,
            links: Vec::new(),
            acl_tx: VecDeque::new(),
            effects: VecDeque::new(),
            last_cmd_ms: now,
            init_deadline: None,
            armed_at: None,
            window_until: None,
            pager: Pager::default(),
            stats: Stats::default(),
            ctrl_reported: false,
            failed: false,
        };
        host.init_step(now, Input::Start);
        host
    }

    // ---- what the shell feeds ------------------------------------------------

    /// An HCI event packet (`code, length, parameters`) from the controller.
    pub fn on_event(&mut self, now: u64, packet: &[u8]) {
        match Event::parse(packet) {
            Ok(event) => {
                self.queue.on_event(&event);
                if let Some(bufs) = &mut self.bufs {
                    bufs.on_completed(&event);
                }
                self.dispatch(now, event);
            }
            Err(_) => self.stats.malformed += 1,
        }
        self.pump(now);
    }

    /// An HCI ACL data packet from the controller.
    pub fn on_acl(&mut self, now: u64, packet: &[u8]) {
        match acl::parse(packet) {
            Ok((header, data)) => {
                if !self.defer_acl(now, header.handle, header.boundary, data) {
                    let actions = self.l2.rx_acl(now, header.handle, header.boundary, data);
                    self.apply(now, actions);
                }
            }
            Err(_) => self.stats.malformed += 1,
        }
        self.pump(now);
    }

    /// Time has reached (or passed) [`Host::next_deadline`].
    pub fn on_tick(&mut self, now: u64) {
        if matches!(self.init.phase(), Phase::FastPath | Phase::SlowPath) {
            self.init_step(now, Input::Tick);
        }
        if !self.failed && self.queue.timed_out(now) {
            self.fail(format!(
                "a command went unanswered for {COMMAND_TIMEOUT_MS} ms"
            ));
        }
        let actions = self.l2.tick(now);
        self.apply(now, actions);
        self.tick_links(now);
        self.pump(now);
    }

    // ---- what the shell drains -----------------------------------------------

    /// The next HCI command packet, if the controller has a credit for it.
    pub fn poll_command(&mut self, now: u64) -> Option<Vec<u8>> {
        let packet = self.queue.pop_ready(now)?;
        self.last_cmd_ms = now;
        Some(packet)
    }

    /// The next HCI ACL packet, if a controller buffer is free for it.
    pub fn poll_acl(&mut self) -> Option<Vec<u8>> {
        let handle = self.acl_tx.front()?.0;
        self.bufs.as_mut()?.acquire(handle).ok()?;
        let (handle, boundary, data) = self.acl_tx.pop_front()?;
        acl::encode(handle, boundary, &data).ok()
    }

    pub fn poll_effect(&mut self) -> Option<Effect> {
        self.effects.pop_front()
    }

    /// The earliest time the host needs [`Host::on_tick`], or `None` when it
    /// can sleep until the next packet.
    pub fn next_deadline(&self) -> Option<u64> {
        if self.failed {
            return None;
        }
        let queue = (self.queue.in_flight() > 0).then(|| self.last_cmd_ms + COMMAND_TIMEOUT_MS);
        [
            self.init_deadline,
            queue,
            self.l2.next_deadline(),
            self.links.iter().filter_map(Link::deadline).min(),
            self.pager.deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    // ---- what the shell and the tests ask --------------------------------------

    pub fn store(&self) -> &BondStore {
        &self.store
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// What the controller turned out to be (valid once bring-up has run).
    pub fn controller(&self) -> &ControllerInfo {
        self.init.info()
    }

    /// The fast path is done: a bonded device can connect now.
    pub fn is_armed(&self) -> bool {
        self.init.is_armed()
    }

    /// Whether a link to `addr` is a registered HID device.
    pub fn is_typing(&self, addr: BdAddr) -> bool {
        self.links.iter().any(|l| l.addr == addr && l.hid.is_some())
    }

    /// The passkey shown for the link to `addr`, while pairing runs.
    pub fn passkey(&self, addr: BdAddr) -> Option<u32> {
        self.links.iter().find(|l| l.addr == addr)?.passkey
    }

    pub fn link_count(&self) -> usize {
        self.links.len()
    }

    // ---- shared plumbing --------------------------------------------------------

    pub(crate) fn note(&mut self, text: alloc::string::String) {
        self.effects.push_back(Effect::Trace(text));
    }

    pub(crate) fn effect(&mut self, effect: Effect) {
        self.effects.push_back(effect);
    }

    pub(crate) fn fail(&mut self, why: alloc::string::String) {
        if !self.failed {
            self.failed = true;
            self.effects.push_back(Effect::Failed(why));
        }
    }

    /// Queue a command; a full queue means a wedged controller.
    pub(crate) fn cmd(&mut self, command: Command) {
        if self.queue.push(&command).is_err() {
            self.stats.overflow += 1;
            self.fail(format!(
                "the command queue is full ({:#06x})",
                command.opcode()
            ));
        }
    }

    pub(crate) fn link_idx(&self, handle: u16) -> Option<usize> {
        self.links.iter().position(|l| l.handle == handle)
    }

    pub(crate) fn link_idx_by_addr(&self, addr: BdAddr) -> Option<usize> {
        self.links.iter().position(|l| l.addr == addr)
    }

    /// Ask the controller to drop a link (once).
    pub(crate) fn disconnect(&mut self, handle: u16, reason: u8) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if self.links[idx].closing {
            return;
        }
        self.links[idx].closing = true;
        self.cmd(Command::Disconnect { handle, reason });
    }

    /// Cut an L2CAP PDU into controller-sized fragments and queue them. A
    /// full queue drops the link: half a PDU would corrupt the stream.
    pub(crate) fn queue_pdu(&mut self, handle: u16, pdu: &[u8]) {
        let max = self
            .bufs
            .as_ref()
            .map_or(DEFAULT_ACL_LEN, |b| usize::from(b.max_len()));
        let pieces = pdu.len().div_ceil(max.max(1));
        if self.acl_tx.len() + pieces > MAX_ACL_QUEUE {
            self.stats.overflow += 1;
            self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
            return;
        }
        for (boundary, piece) in fragments(pdu, max) {
            self.acl_tx.push_back((handle, boundary, piece.to_vec()));
        }
    }

    /// Work that follows any event: the pager may have a page to start.
    fn pump(&mut self, now: u64) {
        self.pager_tick(now);
    }

    // ---- controller bring-up ----------------------------------------------------

    pub(crate) fn init_step(&mut self, now: u64, input: Input<'_>) {
        let was_armed = self.init.is_armed();
        let step = self.init.step(now, input);
        self.init_deadline = step.deadline_ms;
        if let Some(command) = step.send {
            self.cmd(command);
        }
        if self.bufs.is_none() && self.init.info().acl.acl_len != 0 {
            let size = self.init.info().acl;
            self.bufs = Some(AclBuffers::new(size.acl_len, size.acl_num));
        }
        if !was_armed && self.init.is_armed() {
            self.on_armed(now);
        }
        match self.init.phase() {
            Phase::Complete if !self.ctrl_reported => {
                self.ctrl_reported = true;
                self.report_controller();
            }
            Phase::Failed(error) => self.fail(format!("bring-up failed: {error:?}")),
            _ => {}
        }
    }

    /// The fast path is done: say so, open the pairing window and start the
    /// pages that wait for the host (plan decision 5).
    fn on_armed(&mut self, now: u64) {
        self.armed_at = Some(now);
        self.note(format!("READY ms={now}"));
        self.open_pairing_window(now);
        self.pager_arm(now);
    }

    fn report_controller(&mut self) {
        let info = self.init.info().clone();
        let (hci, lmp, vendor) = info.version.map_or((0, 0, 0), |v| {
            (v.hci_version, v.lmp_version, v.manufacturer)
        });
        let addr = info
            .bd_addr
            .map_or(alloc::string::String::from("unknown"), |a| {
                let b = a.to_msb();
                format!("**:**:**:**:{:02X}:{:02X}", b[4], b[5])
            });
        self.note(format!(
            "CTRL:PASS:hci={hci} lmp={lmp} vendor={vendor:#06x} addr={addr}"
        ));
    }

    /// Dispatch a parsed HCI event (`conn.rs`, `pair.rs`).
    fn dispatch(&mut self, now: u64, event: Event) {
        match event {
            Event::CommandComplete { .. } | Event::CommandStatus { .. } => {
                self.on_command_result(now, &event)
            }
            Event::ConnectionRequest {
                addr, link_type, ..
            } => self.on_connection_request(now, addr, link_type),
            Event::ConnectionComplete {
                status,
                handle,
                addr,
                link_type,
                ..
            } => self.on_connection_complete(now, status, handle, addr, link_type),
            Event::DisconnectionComplete {
                status,
                handle,
                reason,
            } => self.on_disconnection(now, status, handle, reason),
            Event::AuthenticationComplete { status, handle } => {
                self.on_authentication(now, status, handle)
            }
            Event::EncryptionChange {
                status,
                handle,
                enabled,
            } => self.on_encryption(now, status, handle, enabled),
            Event::LinkKeyRequest { addr } => self.on_link_key_request(now, addr),
            Event::LinkKeyNotification {
                addr,
                key,
                key_type,
            } => self.on_link_key_notification(addr, key, key_type),
            Event::IoCapabilityRequest { addr } => self.on_io_capability_request(now, addr),
            Event::IoCapabilityResponse {
                addr,
                io_capability,
                ..
            } => self.on_io_capability_response(addr, io_capability),
            Event::UserConfirmationRequest { addr, .. } => self.on_user_confirmation(addr),
            Event::UserPasskeyRequest { addr } => self.on_user_passkey_request(addr),
            Event::UserPasskeyNotification { addr, passkey } => {
                self.on_user_passkey_notification(addr, passkey)
            }
            Event::SimplePairingComplete { status, addr } => {
                self.on_simple_pairing_complete(status, addr)
            }
            _ => {}
        }
    }

    fn on_command_result(&mut self, now: u64, event: &Event) {
        self.init_step(now, Input::Event(event));
        if let Event::CommandStatus {
            status: code,
            opcode,
            ..
        } = event
        {
            if *code != status::SUCCESS {
                self.on_command_refused(now, *opcode, *code);
            }
        }
    }
}

/// `xx:xx:xx:xx:AA:BB`: the form a default-build trace may carry.
pub(crate) fn red(addr: BdAddr) -> bthci::addr::Redacted {
    addr.redacted()
}
