//! The LE half of the link layer: advertising events reaching the host's
//! scan or initiator, the connection, the encryption procedure, and what the
//! user does to an LE device.

use bthci::event::{AdvReport, LeConnection};
use bthci::opcode::status;
use bthci::{BdAddr, Command, Event};
use btsmp::pdu::AddrType;
use btsmp::DeviceAddr;

use crate::ctrl::{LeCreate, Peer, PeerId, SimController, Task};

/// Time between two advertising events of a device.
const ADV_INTERVAL_MS: u64 = 30;
/// How long a touched device keeps advertising.
const ADV_WINDOW_MS: u64 = 10_000;
/// How long a device in pairing mode advertises.
const PAIRING_ADV_MS: u64 = 600_000;
/// ADV_IND: connectable and scannable undirected advertising.
const ADV_IND: u8 = 0;
/// Subevent bits of the LE event mask (bit n = subevent n + 1).
const LE_CONNECTION_COMPLETE_BIT: u8 = 0;
const LE_ADVERTISING_REPORT_BIT: u8 = 1;
/// Connection parameters the link layer reports (30 ms, supervision 4 s).
const INTERVAL: u16 = 0x18;
const SUPERVISION: u16 = 0x190;

impl SimController {
    /// Deliver an LE Meta event unless the host masked that subevent.
    fn emit_le(&mut self, bit: u8, ev: Event) {
        if self.le_event_mask >> bit & 1 != 0 {
            self.emit(ev);
        }
    }

    // ---- commands ----------------------------------------------------------

    pub(crate) fn le_create_connection(&mut self, op: u16, create: LeCreate) {
        if self.le_create.is_some() {
            return self.status(op, status::COMMAND_DISALLOWED);
        }
        self.le_create = Some(create);
        self.status(op, 0);
    }

    pub(crate) fn le_create_cancel(&mut self, op: u16) {
        if self.le_create.take().is_none() {
            return self.complete(op, vec![status::COMMAND_DISALLOWED]);
        }
        self.complete(op, vec![0]);
        // A cancelled initiator ends with a failed LE Connection Complete.
        let d = self.followup(op, 1);
        let ev = Event::LeConnectionComplete(LeConnection {
            status: status::UNKNOWN_CONNECTION,
            handle: 0,
            role: 0,
            peer_address_type: 0,
            peer: BdAddr::default(),
            interval: 0,
            latency: 0,
            supervision_timeout: 0,
            clock_accuracy: 0,
        });
        self.after(d, Task::Emit(ev));
    }

    pub(crate) fn le_start_encryption(&mut self, op: u16, handle: u16, ltk: [u8; 16]) {
        let is_le = self
            .conn_index(handle)
            .is_some_and(|ci| matches!(self.peers[self.conns[ci].peer], Peer::Le(_)));
        if !is_le {
            return self.status(op, status::UNKNOWN_CONNECTION);
        }
        self.status(op, 0);
        let d = self.followup(op, 3);
        self.after(d, Task::LeEncrypt { handle, ltk });
    }

    /// The harness-only passkey hook (`bthci::opcode::HARNESS_SHOW_PASSKEY`):
    /// the user reads the passkey off the host's screen and types it.
    pub(crate) fn show_passkey(&mut self, op: u16, params: &[u8]) {
        let Some(shown) = params
            .get(..4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        else {
            return self.complete(op, vec![status::INVALID_PARAMETERS]);
        };
        self.complete(op, vec![0]);
        let waiting: Vec<(usize, u16)> = (0..self.peers.len())
            .filter_map(|i| match &self.peers[i] {
                Peer::Le(p) => p.handle().map(|h| (i, h)),
                Peer::Br(_) => None,
            })
            .collect();
        for (peer, handle) in waiting {
            let pdus = self.le_at(peer).show_passkey(shown);
            self.peer_sends(handle, pdus);
        }
    }

    // ---- advertising and connecting ----------------------------------------

    /// One advertising event: a pending initiator that matches connects, a
    /// scanning host hears a report, otherwise nobody is listening.
    pub(crate) fn le_advertise(&mut self, peer: usize) {
        let clock = self.clock;
        let p = self.le_at(peer);
        let Some(until) = p.adv_until else { return };
        if p.conn.is_some() || clock > until {
            p.adv_until = None;
            return;
        }
        let (address_type, addr) = p.adv;
        let data = p.adv_data();
        let hit = self.le_create.is_some_and(|c| match c.filter_policy {
            0 => c.peer_type == address_type && c.peer == addr,
            _ => self.accept_list.contains(&(address_type, addr)),
        });
        if hit {
            self.le_create = None;
            self.after(
                1,
                Task::LeConnected {
                    peer,
                    address_type,
                    addr,
                },
            );
            return;
        }
        if self.le_scanning {
            let report = AdvReport {
                event_type: ADV_IND,
                address_type,
                addr,
                data,
                rssi: -50,
            };
            self.emit_le(
                LE_ADVERTISING_REPORT_BIT,
                Event::LeAdvertisingReport(vec![report]),
            );
        }
        self.after(ADV_INTERVAL_MS, Task::LeAdv { peer });
    }

    /// Start (or extend) advertising of `peer` for `window` ms.
    fn le_start_adv(&mut self, peer: usize, window: u64) {
        let until = self.clock + window;
        let p = self.le_at(peer);
        let fresh = p.adv_until.is_none();
        p.start_advertising(until);
        if fresh && p.conn.is_none() {
            self.after(0, Task::LeAdv { peer });
        }
    }

    pub(crate) fn le_connected(&mut self, peer: usize, address_type: u8, addr: BdAddr) {
        if self.le_at(peer).is_connected() {
            return;
        }
        let handle = self.next_handle_for_le();
        self.conns.push(crate::ctrl::Conn {
            handle,
            peer,
            by_peer: false,
            auth: crate::ctrl::Auth::Idle,
            authenticated: false,
            encrypted: false,
            passkey: 0,
        });
        let kind = if address_type == 0 {
            AddrType::Public
        } else {
            AddrType::Random
        };
        let own = DeviceAddr { kind, addr: addr.0 };
        let central = DeviceAddr {
            kind: AddrType::Public,
            addr: self.cfg.bd_addr.0,
        };
        self.le_at(peer).link_up(handle, own, central);
        let conn = LeConnection {
            status: 0,
            handle,
            role: 0,
            peer_address_type: address_type,
            peer: addr,
            interval: INTERVAL,
            latency: 0,
            supervision_timeout: SUPERVISION,
            clock_accuracy: 0,
        };
        self.emit_le(
            LE_CONNECTION_COMPLETE_BIT,
            Event::LeConnectionComplete(conn),
        );
        let delay = self.le_at(peer).greet_delay_ms;
        self.after(delay, Task::LeGreet { handle });
    }

    fn next_handle_for_le(&mut self) -> u16 {
        // Handles are shared with BR/EDR links.
        let mut h = self.next_handle;
        while self.conn_index(h).is_some() {
            h = if h >= 0x0EFF { 0x0040 } else { h + 1 };
        }
        self.next_handle = if h >= 0x0EFF { 0x0040 } else { h + 1 };
        h
    }

    pub(crate) fn le_greet(&mut self, handle: u16) {
        let Some(ci) = self.conn_index(handle) else {
            return;
        };
        let peer = self.conns[ci].peer;
        let pdus = self.le_at(peer).greeting();
        self.peer_sends(handle, pdus);
    }

    // ---- encryption --------------------------------------------------------

    pub(crate) fn le_encrypt(&mut self, handle: u16, ltk: [u8; 16]) {
        let Some(ci) = self.conn_index(handle) else {
            return;
        };
        let peer = self.conns[ci].peer;
        if !self.le_at(peer).accept_ltk(&ltk) {
            // The peripheral's host has no such key.
            return self.emit(Event::EncryptionChange {
                status: status::PIN_OR_KEY_MISSING,
                handle,
                enabled: false,
            });
        }
        self.conns[ci].encrypted = true;
        let pdus = self.le_at(peer).on_encrypted();
        let change = Event::EncryptionChange {
            status: 0,
            handle,
            enabled: true,
        };
        if self.cfg.acl_beats_encryption {
            // The data goes first and the event trails it.
            self.peer_sends(handle, pdus);
            self.after(4, Task::Emit(change));
        } else {
            self.emit(change);
            self.peer_sends(handle, pdus);
        }
    }

    // ---- what the user does -------------------------------------------------

    /// The user touches an LE device: a bonded one starts advertising.
    pub fn le_wake(&mut self, id: PeerId, now_ms: u64) {
        self.run_until(now_ms);
        let p = self.le_at(id.0);
        let advertises = p.bond.is_some() && !p.is_connected();
        if advertises {
            self.le_start_adv(id.0, ADV_WINDOW_MS);
        }
    }

    /// The user holds the pairing button: the device forgets its bond and
    /// advertises.
    pub fn le_enter_pairing_mode(&mut self, id: PeerId, now_ms: u64) {
        self.run_until(now_ms);
        self.le_at(id.0).set_pairing_mode(true);
        self.le_start_adv(id.0, PAIRING_ADV_MS);
    }

    /// Resume advertising after a controller reset (the device does not know
    /// the host reset).
    pub(crate) fn le_resume_advertising(&mut self) {
        for peer in 0..self.peers.len() {
            if matches!(self.peers[peer], Peer::Le(_)) && self.le_at(peer).adv_until.is_some() {
                self.after(ADV_INTERVAL_MS, Task::LeAdv { peer });
            }
        }
    }

    /// An Encryption Change or Command to dispatch from the generic command
    /// table.
    pub(crate) fn le_command(&mut self, cmd: &Command) -> bool {
        let op = cmd.opcode();
        match cmd {
            Command::LeCreateConnection {
                filter_policy,
                peer_address_type,
                peer,
                ..
            } => self.le_create_connection(
                op,
                LeCreate {
                    filter_policy: *filter_policy,
                    peer_type: *peer_address_type,
                    peer: *peer,
                },
            ),
            Command::LeCreateConnectionCancel => self.le_create_cancel(op),
            Command::LeSetScanParameters { .. } => self.complete(op, vec![0]),
            Command::LeSetScanEnable { enable, .. } => {
                self.le_scanning = *enable;
                self.complete(op, vec![0]);
            }
            Command::LeStartEncryption { handle, ltk, .. } => {
                self.le_start_encryption(op, *handle, *ltk)
            }
            Command::Unknown { opcode, params }
                if *opcode == bthci::opcode::HARNESS_SHOW_PASSKEY =>
            {
                self.show_passkey(op, params)
            }
            _ => return false,
        }
        true
    }
}
