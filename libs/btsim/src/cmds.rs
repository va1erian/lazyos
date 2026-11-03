//! HCI command handling: parse, check credits, answer.

use bthci::opcode::status;
use bthci::{ret, BdAddr, Command, Event};

use crate::ctrl::{
    Auth, SimController, Task, Violation, DEFAULT_EVENT_MASK, DEFAULT_LE_EVENT_MASK,
};

const LE_BUFFER: ret::BufferSize = ret::BufferSize {
    acl_len: 251,
    acl_num: 8,
};
/// Accept-list capacity (Core allows controllers as few as 1; real ones 8+).
const ACCEPT_LIST_MAX: usize = 8;
/// Generous Page Timeout: how long a page to an absent device takes.
const PAGE_TIMEOUT_MS: u64 = 2_000;

impl SimController {
    pub(crate) fn handle_packet(&mut self, packet: &[u8]) {
        let cmd = match Command::parse(packet) {
            Ok(c) => c,
            Err(_) => {
                self.violations.push(Violation::MalformedCommand);
                if let Some(op) = packet.get(..2) {
                    let opcode = u16::from_le_bytes([op[0], op[1]]);
                    self.complete(opcode, vec![status::INVALID_PARAMETERS]);
                }
                return;
            }
        };
        if self.credits == 0 {
            self.violations.push(Violation::CommandWithoutCredit {
                opcode: cmd.opcode(),
            });
        }
        self.credits = self.credits.saturating_sub(1);
        self.log_command(&cmd);
        self.dispatch(cmd);
    }

    fn latency(&self, opcode: u16) -> u64 {
        if opcode == bthci::opcode::RESET {
            self.cfg.reset_latency_ms
        } else {
            self.cfg.cmd_latency_ms
        }
    }

    /// Answer with Command Complete after the command's latency.
    pub(crate) fn complete(&mut self, opcode: u16, return_params: Vec<u8>) {
        let d = self.latency(opcode);
        let ev = Event::CommandComplete {
            num_commands: 1,
            opcode,
            return_params,
        };
        self.after(d, Task::Emit(ev));
    }

    /// Answer with Command Status.
    pub(crate) fn status(&mut self, opcode: u16, st: u8) {
        let d = self.latency(opcode);
        self.after(
            d,
            Task::Emit(Event::CommandStatus {
                status: st,
                num_commands: 1,
                opcode,
            }),
        );
    }

    /// Delay for a follow-up event so it lands after the command's answer.
    pub(crate) fn followup(&self, opcode: u16, extra: u64) -> u64 {
        self.latency(opcode) + extra
    }

    fn complete_with_addr(&mut self, opcode: u16, st: u8, addr: BdAddr) {
        let mut rp = vec![st];
        rp.extend_from_slice(&addr.0);
        self.complete(opcode, rp);
    }

    pub(crate) fn reset(&mut self) {
        // A peer that is paging the host does not know the host reset, so it
        // keeps paging; everything the controller itself had scheduled is void.
        let paging: Vec<usize> = (0..self.peers.len())
            .filter(|&i| self.br_at(i).paging_until.is_some())
            .collect();
        for i in 0..self.peers.len() {
            self.br_at(i).link_down();
        }
        self.drop_timeline();
        for peer in paging {
            self.after(crate::link::PAGE_RETRY_MS, Task::PeerPage { peer });
        }
        self.conns.clear();
        self.pending_requests.clear();
        self.acl_in_use = 0;
        self.event_mask = DEFAULT_EVENT_MASK;
        self.le_event_mask = DEFAULT_LE_EVENT_MASK;
        self.scan_enable = 0;
        self.ssp = false;
        self.class_of_device = 0;
        self.local_name.clear();
        self.accept_list.clear();
        self.le_connecting = false;
    }

    fn dispatch(&mut self, cmd: Command) {
        use Command::*;
        let op = cmd.opcode();
        if !self.cfg.le_supported && op >> 10 == bthci::opcode::OGF_LE {
            return self.complete(op, vec![status::UNKNOWN_COMMAND]);
        }
        match cmd {
            Reset => {
                self.reset();
                self.complete(op, vec![0]);
            }
            ReadBdAddr => self.complete(op, ret::encode_bd_addr(self.cfg.bd_addr)),
            ReadLocalVersion => {
                let v = ret::LocalVersion {
                    hci_version: 9,
                    hci_revision: 0x1234,
                    lmp_version: 9,
                    manufacturer: 0xFFFF,
                    lmp_subversion: 1,
                };
                self.complete(op, ret::encode_local_version(&v));
            }
            ReadBufferSize => {
                let b = ret::BufferSize {
                    acl_len: self.cfg.acl_len,
                    acl_num: self.cfg.acl_num,
                };
                self.complete(op, ret::encode_buffer_size(b));
            }
            LeReadBufferSize => self.complete(op, ret::encode_le_buffer_size(LE_BUFFER)),
            SetEventMask(m) => {
                self.event_mask = m;
                self.complete(op, vec![0]);
            }
            LeSetEventMask(m) => {
                self.le_event_mask = m;
                self.complete(op, vec![0]);
            }
            WriteScanEnable(v) => {
                self.scan_enable = v & 3;
                self.complete(op, vec![0]);
            }
            WriteSimplePairingMode(on) => {
                self.ssp = on;
                self.complete(op, vec![0]);
            }
            WriteClassOfDevice(c) => {
                self.class_of_device = c;
                self.complete(op, vec![0]);
            }
            WriteLocalName(n) => {
                self.local_name = n;
                self.complete(op, vec![0]);
            }
            LeSetScanParameters { .. } | LeSetScanEnable { .. } | LeCreateConnectionCancel => {
                self.le_connecting = false;
                self.complete(op, vec![0]);
            }
            LeClearAcceptList => {
                self.accept_list.clear();
                self.complete(op, vec![0]);
            }
            LeAddDeviceToAcceptList { address_type, addr } => {
                let full = self.accept_list.len() >= ACCEPT_LIST_MAX;
                if !full && !self.accept_list.contains(&(address_type, addr)) {
                    self.accept_list.push((address_type, addr));
                }
                self.complete(
                    op,
                    vec![if full {
                        status::CONNECTION_REJECTED_RESOURCES
                    } else {
                        0
                    }],
                );
            }
            LeCreateConnection { .. } => {
                self.le_connecting = true;
                self.status(op, 0);
            }
            LeStartEncryption { .. } => self.status(op, status::UNKNOWN_CONNECTION),
            LeLongTermKeyRequestReply { handle, .. }
            | LeLongTermKeyRequestNegativeReply { handle } => {
                let mut rp = vec![status::UNKNOWN_CONNECTION];
                rp.extend_from_slice(&handle.to_le_bytes());
                self.complete(op, rp);
            }
            CreateConnection { addr, .. } => self.create_connection(op, addr),
            AcceptConnectionRequest { addr, .. } => self.accept(op, addr),
            RejectConnectionRequest { addr, .. } => self.reject(op, addr),
            AuthenticationRequested { handle } => self.authentication_requested(op, handle),
            SetConnectionEncryption { handle, enable } => self.set_encryption(op, handle, enable),
            LinkKeyRequestReply { addr, key } => self.link_key_reply(op, addr, Some(key)),
            LinkKeyRequestNegativeReply { addr } => self.link_key_reply(op, addr, None),
            IoCapabilityRequestReply {
                addr,
                io_capability,
                ..
            } => self.io_capability_reply(op, addr, io_capability),
            IoCapabilityRequestNegativeReply { addr, reason } => {
                self.pairing_refused(op, addr, reason)
            }
            UserConfirmationRequestReply { addr } => self.confirmation_reply(op, addr, true),
            UserConfirmationRequestNegativeReply { addr } => {
                self.confirmation_reply(op, addr, false)
            }
            UserPasskeyRequestReply { addr, .. } | UserPasskeyRequestNegativeReply { addr } => {
                // No peer asks the host to type a passkey.
                self.complete_with_addr(op, status::COMMAND_DISALLOWED, addr);
            }
            Disconnect { handle, reason: _ } => {
                if self.conn_index(handle).is_none() {
                    return self.status(op, status::UNKNOWN_CONNECTION);
                }
                self.status(op, 0);
                let d = self.followup(op, 2);
                self.after(
                    d,
                    Task::Disconnected {
                        handle,
                        reason: status::CONNECTION_TERMINATED_BY_LOCAL_HOST,
                    },
                );
            }
            Unknown { .. } => self.status(op, status::UNKNOWN_COMMAND),
        }
    }

    fn create_connection(&mut self, op: u16, addr: BdAddr) {
        if self.conn_by_addr(addr).is_some() {
            return self.status(op, 0x0B);
        }
        self.status(op, 0);
        let found = self
            .peer_index(addr)
            .filter(|&i| self.br_at(i).connectable());
        match found {
            Some(peer) => {
                let d = self.followup(op, self.cfg.page_ms);
                self.after(
                    d,
                    Task::Connected {
                        peer,
                        peer_initiated: false,
                    },
                );
            }
            None => {
                let d = self.followup(op, PAGE_TIMEOUT_MS);
                self.after(
                    d,
                    Task::ConnectFailed {
                        addr,
                        status: status::PAGE_TIMEOUT,
                    },
                );
            }
        }
    }

    fn accept(&mut self, op: u16, addr: BdAddr) {
        let Some(at) = self
            .pending_requests
            .iter()
            .position(|&i| self.peers[i].addr() == addr)
        else {
            return self.status(op, status::COMMAND_DISALLOWED);
        };
        let peer = self.pending_requests.remove(at);
        self.status(op, 0);
        let d = self.followup(op, 5);
        self.after(
            d,
            Task::Connected {
                peer,
                peer_initiated: true,
            },
        );
    }

    fn reject(&mut self, op: u16, addr: BdAddr) {
        let Some(at) = self
            .pending_requests
            .iter()
            .position(|&i| self.peers[i].addr() == addr)
        else {
            return self.status(op, status::COMMAND_DISALLOWED);
        };
        let peer = self.pending_requests.remove(at);
        self.br_at(peer).paging_until = None;
        self.status(op, 0);
    }

    fn authentication_requested(&mut self, op: u16, handle: u16) {
        if self.conn_index(handle).is_none() {
            return self.status(op, status::UNKNOWN_CONNECTION);
        }
        self.status(op, 0);
        let d = self.followup(op, 1);
        self.after(
            d,
            Task::AuthStart {
                handle,
                by_peer: false,
            },
        );
    }

    fn set_encryption(&mut self, op: u16, handle: u16, enable: bool) {
        let Some(ci) = self.conn_index(handle) else {
            return self.status(op, status::UNKNOWN_CONNECTION);
        };
        if enable && !self.conns[ci].authenticated {
            return self.status(op, status::COMMAND_DISALLOWED);
        }
        self.status(op, 0);
        let d = self.followup(op, 2);
        self.after(d, Task::Encrypt { handle, on: enable });
    }

    fn link_key_reply(&mut self, op: u16, addr: BdAddr, key: Option<[u8; 16]>) {
        let waiting = self
            .conn_by_addr(addr)
            .filter(|&ci| matches!(self.conns[ci].auth, Auth::WaitLinkKey { .. }));
        let Some(ci) = waiting else {
            return self.complete_with_addr(op, status::COMMAND_DISALLOWED, addr);
        };
        self.complete_with_addr(op, 0, addr);
        let d = self.followup(op, 1);
        self.resolve_link_key(ci, key, d);
    }

    fn io_capability_reply(&mut self, op: u16, addr: BdAddr, host_io: u8) {
        let waiting = self
            .conn_by_addr(addr)
            .filter(|&ci| self.conns[ci].auth == Auth::WaitIoCap);
        let Some(ci) = waiting else {
            return self.complete_with_addr(op, status::COMMAND_DISALLOWED, addr);
        };
        self.complete_with_addr(op, 0, addr);
        let d = self.followup(op, 1);
        self.begin_association(ci, host_io, d);
    }

    fn pairing_refused(&mut self, op: u16, addr: BdAddr, reason: u8) {
        let waiting = self
            .conn_by_addr(addr)
            .filter(|&ci| self.conns[ci].auth == Auth::WaitIoCap);
        let Some(ci) = waiting else {
            return self.complete_with_addr(op, status::COMMAND_DISALLOWED, addr);
        };
        self.complete_with_addr(op, 0, addr);
        let handle = self.conns[ci].handle;
        self.conns[ci].auth = Auth::Idle;
        let d = self.followup(op, 1);
        self.after(
            d,
            Task::PairingResult {
                handle,
                status: reason.max(1),
                key_type: 0,
            },
        );
    }

    fn confirmation_reply(&mut self, op: u16, addr: BdAddr, yes: bool) {
        let waiting = self
            .conn_by_addr(addr)
            .filter(|&ci| self.conns[ci].auth == Auth::WaitConfirm);
        let Some(ci) = waiting else {
            return self.complete_with_addr(op, status::COMMAND_DISALLOWED, addr);
        };
        self.complete_with_addr(op, 0, addr);
        let handle = self.conns[ci].handle;
        self.conns[ci].auth = Auth::Idle;
        let d = self.followup(op, 1);
        self.after(
            d,
            Task::PairingResult {
                handle,
                status: if yes {
                    0
                } else {
                    status::AUTHENTICATION_FAILURE
                },
                key_type: 4,
            },
        );
    }
}
