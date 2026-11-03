//! The link manager: connections, authentication, Secure Simple Pairing and
//! encryption between the host's controller and a peer, run from the timeline.

use bthci::opcode::status;
use bthci::Event;

use crate::ctrl::{Auth, Conn, Peer, SimController, Task};
use crate::peer::PasskeyBehavior;

/// Time between a paging attempt and the next while page scan is off.
pub(crate) const PAGE_RETRY_MS: u64 = 10;
const ACCEPT_WINDOW_MS: u64 = 5_000;
/// How long the user takes to type a passkey.
const TYPING_MS: u64 = 500;
const LINK_ACL: u8 = 1;
/// Connection Accept Timeout Exceeded.
const ACCEPT_TIMEOUT: u8 = 0x10;

impl SimController {
    pub(crate) fn execute(&mut self, task: Task) {
        match task {
            Task::Emit(ev) => self.emit(ev),
            Task::Inquiry(ev) => {
                let done = matches!(&ev, Event::Unknown { code, .. } if *code == bthci::inquiry::EV_INQUIRY_COMPLETE);
                self.inquiry_running &= !done;
                self.emit(ev)
            }
            Task::EmitAcl(packet) => self.emit_acl(packet),
            Task::Connected {
                peer,
                peer_initiated,
            } => self.connected(peer, peer_initiated),
            Task::ConnectFailed { addr, status } => {
                self.emit(Event::ConnectionComplete {
                    status,
                    handle: 0,
                    addr,
                    link_type: LINK_ACL,
                    encryption_enabled: false,
                });
            }
            Task::PeerPage { peer } => self.peer_page(peer),
            Task::AcceptTimeout { peer } => self.accept_timeout(peer),
            Task::AuthStart { handle, by_peer } => {
                if let Some(ci) = self.conn_index(handle) {
                    self.conns[ci].auth = Auth::WaitLinkKey { by_peer };
                    let addr = self.peers[self.conns[ci].peer].addr();
                    self.emit(Event::LinkKeyRequest { addr });
                }
            }
            Task::PasskeyTyped { handle } => self.passkey_typed(handle),
            Task::PairingResult {
                handle,
                status,
                key_type,
            } => self.pairing_result(handle, status, key_type),
            Task::Encrypt { handle, on } => self.encrypt(handle, on),
            Task::Disconnected { handle, reason } => self.disconnected(handle, reason),
            Task::AclDone { handle } => {
                self.acl_in_use = self.acl_in_use.saturating_sub(1);
                if self.conn_index(handle).is_some() {
                    self.emit(Event::NumberOfCompletedPackets(vec![(handle, 1)]));
                }
            }
            Task::LeAdv { peer } => self.le_advertise(peer),
            Task::LeConnected {
                peer,
                address_type,
                addr,
            } => self.le_connected(peer, address_type, addr),
            Task::LeGreet { handle } => self.le_greet(handle),
            Task::LeEncrypt { handle, ltk } => self.le_encrypt(handle, ltk),
            Task::PeerChannels { handle } => {
                if let Some(ci) = self.conn_index(handle) {
                    let (peer, now) = (self.conns[ci].peer, self.clock);
                    let pdus = self.br_at(peer).start_channels(now);
                    self.peer_sends(handle, pdus);
                }
            }
        }
    }

    fn new_handle(&mut self) -> u16 {
        loop {
            let h = self.next_handle;
            self.next_handle = if h >= 0x0EFF { 0x0040 } else { h + 1 };
            if self.conn_index(h).is_none() {
                return h;
            }
        }
    }

    fn connected(&mut self, peer: usize, peer_initiated: bool) {
        if self.br_at(peer).is_connected() {
            return;
        }
        let handle = self.new_handle();
        self.conns.push(Conn {
            handle,
            peer,
            by_peer: peer_initiated,
            auth: Auth::Idle,
            authenticated: false,
            encrypted: false,
            passkey: 0,
        });
        self.br_at(peer).link_up(handle, peer_initiated);
        let addr = self.peers[peer].addr();
        self.emit(Event::ConnectionComplete {
            status: 0,
            handle,
            addr,
            link_type: LINK_ACL,
            encryption_enabled: false,
        });
        if peer_initiated {
            self.after(
                2,
                Task::AuthStart {
                    handle,
                    by_peer: true,
                },
            );
        }
    }

    /// One paging attempt by a peer that wants to reconnect.
    fn peer_page(&mut self, peer: usize) {
        let page_scan = self.scan_enable & 2 != 0;
        let clock = self.clock;
        let p = self.br_at(peer);
        let Some(until) = p.paging_until else { return };
        if p.is_connected() || clock > until {
            p.paging_until = None;
            return;
        }
        if !page_scan || self.pending_requests.contains(&peer) {
            return self.after(PAGE_RETRY_MS, Task::PeerPage { peer });
        }
        let (addr, class) = (
            self.peers[peer].addr(),
            self.br_at(peer).kind.class_of_device(),
        );
        self.pending_requests.push(peer);
        self.emit(Event::ConnectionRequest {
            addr,
            class_of_device: class,
            link_type: LINK_ACL,
        });
        self.after(ACCEPT_WINDOW_MS, Task::AcceptTimeout { peer });
    }

    fn accept_timeout(&mut self, peer: usize) {
        let Some(at) = self.pending_requests.iter().position(|&i| i == peer) else {
            return;
        };
        self.pending_requests.remove(at);
        self.br_at(peer).paging_until = None;
        let addr = self.peers[peer].addr();
        self.emit(Event::ConnectionComplete {
            status: ACCEPT_TIMEOUT,
            handle: 0,
            addr,
            link_type: LINK_ACL,
            encryption_enabled: false,
        });
    }

    /// The host answered (or refused) the link key request.
    pub(crate) fn resolve_link_key(&mut self, ci: usize, key: Option<[u8; 16]>, d: u64) {
        let handle = self.conns[ci].handle;
        let peer = self.conns[ci].peer;
        let Auth::WaitLinkKey { by_peer } = self.conns[ci].auth else {
            return;
        };
        self.conns[ci].auth = Auth::Idle;
        let addr = self.peers[peer].addr();
        let (stored, pairable, ssp) = {
            let p = self.br_at(peer);
            (p.link_key, p.pairing_mode, self.ssp)
        };
        let failure = match (key, stored) {
            (Some(k), Some((pk, _))) if k == pk => None,
            (Some(_), Some(_)) => Some(status::AUTHENTICATION_FAILURE),
            (Some(_), None) => Some(status::PIN_OR_KEY_MISSING),
            (None, _) if !by_peer && pairable && ssp => {
                self.conns[ci].auth = Auth::WaitIoCap;
                return self.after(d, Task::Emit(Event::IoCapabilityRequest { addr }));
            }
            (None, _) => Some(status::PIN_OR_KEY_MISSING),
        };
        match (failure, by_peer) {
            (None, true) => {
                self.conns[ci].authenticated = true;
                self.after(d, Task::Encrypt { handle, on: true });
            }
            (None, false) => {
                self.conns[ci].authenticated = true;
                self.after(
                    d,
                    Task::Emit(Event::AuthenticationComplete { status: 0, handle }),
                );
            }
            // A peer that fails to authenticate the host drops the link.
            (Some(reason), true) => self.after(d, Task::Disconnected { handle, reason }),
            (Some(st), false) => self.after(
                d,
                Task::Emit(Event::AuthenticationComplete { status: st, handle }),
            ),
        }
    }

    /// IO capabilities are exchanged: pick the association model.
    pub(crate) fn begin_association(&mut self, ci: usize, host_io: u8, d: u64) {
        let (handle, peer) = (self.conns[ci].handle, self.conns[ci].peer);
        let addr = self.peers[peer].addr();
        let p = self.br_at(peer);
        let (peer_io, behavior) = (p.io_capability, p.passkey);
        let auth_req = if peer_io == 3 { 0x04 } else { 0x05 };
        self.after(
            d,
            Task::Emit(Event::IoCapabilityResponse {
                addr,
                io_capability: peer_io,
                oob_present: 0,
                authentication_requirements: auth_req,
            }),
        );
        match (peer_io, host_io) {
            // The peer has a keyboard, the host a display: Passkey Entry.
            (2, 0 | 1) => {
                let passkey = (self.random() % 1_000_000) as u32;
                self.conns[ci].auth = Auth::WaitPasskey;
                self.conns[ci].passkey = passkey;
                self.after(
                    d + 1,
                    Task::Emit(Event::UserPasskeyNotification { addr, passkey }),
                );
                if behavior != PasskeyBehavior::Never {
                    self.after(d + 1 + TYPING_MS, Task::PasskeyTyped { handle });
                }
            }
            // Either side without input or output: Just Works. A host that can
            // show a yes/no is still asked, without a number to compare.
            (3, _) | (_, 3) | (1, 1) if host_io == 1 => {
                self.conns[ci].auth = Auth::WaitConfirm;
                let numeric_value = (self.random() % 1_000_000) as u32;
                self.after(
                    d + 1,
                    Task::Emit(Event::UserConfirmationRequest {
                        addr,
                        numeric_value,
                    }),
                );
            }
            (3, _) | (_, 3) => self.after(
                d + 1,
                Task::PairingResult {
                    handle,
                    status: 0,
                    key_type: 4,
                },
            ),
            _ => self.after(
                d + 1,
                Task::PairingResult {
                    handle,
                    status: status::PAIRING_NOT_ALLOWED,
                    key_type: 0,
                },
            ),
        }
    }

    fn passkey_typed(&mut self, handle: u16) {
        let Some(ci) = self
            .conn_index(handle)
            .filter(|&ci| self.conns[ci].auth == Auth::WaitPasskey)
        else {
            return;
        };
        self.conns[ci].auth = Auth::Idle;
        let ok = self.br_at(self.conns[ci].peer).passkey == PasskeyBehavior::Correct;
        let st = if ok {
            0
        } else {
            status::AUTHENTICATION_FAILURE
        };
        self.after(
            0,
            Task::PairingResult {
                handle,
                status: st,
                key_type: 5,
            },
        );
    }

    fn pairing_result(&mut self, handle: u16, st: u8, key_type: u8) {
        let Some(ci) = self.conn_index(handle) else {
            return;
        };
        let peer = self.conns[ci].peer;
        let addr = self.peers[peer].addr();
        self.conns[ci].auth = Auth::Idle;
        if st == 0 {
            let mut key = [0u8; 16];
            for half in key.chunks_mut(8) {
                half.copy_from_slice(&self.random().to_le_bytes());
            }
            let p = self.br_at(peer);
            p.link_key = Some((key, key_type));
            p.pairing_mode = false;
            self.conns[ci].authenticated = true;
            self.emit(Event::SimplePairingComplete { status: 0, addr });
            self.emit(Event::LinkKeyNotification {
                addr,
                key,
                key_type,
            });
            self.emit(Event::AuthenticationComplete { status: 0, handle });
        } else {
            self.emit(Event::SimplePairingComplete { status: st, addr });
            self.emit(Event::AuthenticationComplete { status: st, handle });
        }
    }

    fn encrypt(&mut self, handle: u16, on: bool) {
        let Some(ci) = self.conn_index(handle) else {
            return;
        };
        self.conns[ci].encrypted = on;
        let (peer, by_peer) = (self.conns[ci].peer, self.conns[ci].by_peer);
        self.br_at(peer).set_encrypted(on);
        self.emit(Event::EncryptionChange {
            status: 0,
            handle,
            enabled: on,
        });
        if on && by_peer {
            self.after(1, Task::PeerChannels { handle });
        }
    }

    fn disconnected(&mut self, handle: u16, reason: u8) {
        let Some(ci) = self.conn_index(handle) else {
            return;
        };
        let conn = self.conns.remove(ci);
        match &mut self.peers[conn.peer] {
            Peer::Br(p) => p.link_down(),
            Peer::Le(p) => p.link_down(),
        }
        self.emit(Event::DisconnectionComplete {
            status: 0,
            handle,
            reason,
        });
    }
}
