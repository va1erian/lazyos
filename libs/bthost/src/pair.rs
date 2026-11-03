//! Secure Simple Pairing and the bond store (docs/bluetooth-plan.md decisions
//! 4 and 6).
//!
//! The host declares IO capability DisplayYesNo with MITM protection wanted.
//! A mouse (NoInputNoOutput) therefore pairs with Just Works (key type 4,
//! unauthenticated), and a keyboard (KeyboardOnly) with Passkey Entry (key
//! type 5 or 8, authenticated): the host shows the passkey, the user types it
//! on the keyboard. Numeric comparison with a display-capable peer needs a
//! person to say yes and waits for B5's interface; until then it is refused.
//!
//! Only key types 4, 5, 7 and 8 are bonds. Secure Connections host support
//! is not enabled (`Write_Secure_Connections_Host_Support` is not sent), so
//! a controller never produces 7 or 8 and an authenticated key is type 5; 8
//! is accepted so enabling it later is a one-line change in the init script.
//! Legacy combination keys (PIN pairing) and debug keys (public) are refused.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use btbond::{AddrKind, Bond, Gatt, InputClass, LinkKey};
use bthci::opcode::status;
use bthci::{BdAddr, Command};

use crate::effect::Effect;
use crate::host::{red, Host};

/// HCI IO capability: DisplayYesNo.
const IO_DISPLAY_YES_NO: u8 = 1;
/// HCI IO capability: NoInputNoOutput (the peer for Just Works).
const IO_NO_INPUT_NO_OUTPUT: u8 = 3;
/// Authentication requirements: MITM protection, general bonding.
const AUTH_MITM_GENERAL_BONDING: u8 = 5;
/// The key size SSP produces.
const SSP_KEY_SIZE: u8 = 16;

/// Link key types (Core spec Vol 4 Part E 7.7.24).
mod key_type {
    pub const CHANGED_COMBINATION: u8 = 6;
    pub const UNAUTHENTICATED_P192: u8 = 4;
    pub const AUTHENTICATED_P192: u8 = 5;
    pub const UNAUTHENTICATED_P256: u8 = 7;
    pub const AUTHENTICATED_P256: u8 = 8;
}

/// Whether a link key type makes a bond.
pub fn key_type_accepted(key_type: u8) -> bool {
    matches!(
        key_type,
        key_type::UNAUTHENTICATED_P192
            | key_type::AUTHENTICATED_P192
            | key_type::UNAUTHENTICATED_P256
            | key_type::AUTHENTICATED_P256
    )
}

/// Whether the key was made with MITM protection (Passkey Entry or Numeric
/// Comparison): the only keys a keyboard may type with.
pub fn key_authenticated(key_type: u8) -> bool {
    matches!(
        key_type,
        key_type::AUTHENTICATED_P192 | key_type::AUTHENTICATED_P256
    )
}

impl Host {
    pub(crate) fn save_bonds(&mut self) {
        match self.store.encode() {
            Ok(bytes) => self.effect(Effect::SaveBonds(bytes)),
            Err(error) => self.note(format!("BOND:SAVE:FAIL reason={error:?}")),
        }
    }

    /// Forget a device (B5's `Forget`, and a device's own virtual cable
    /// unplug): the bond goes, the link is dropped, nothing pages it again.
    pub fn forget(&mut self, now: u64, addr: BdAddr) {
        if self.store.remove(addr, AddrKind::BrEdr) {
            self.save_bonds();
            self.note(format!("BOND:FORGET addr={}", red(addr)));
        }
        self.pager_forget(addr);
        if let Some(idx) = self.link_idx_by_addr(addr) {
            let handle = self.links[idx].handle;
            self.release_hid(handle);
            self.disconnect(handle, status::REMOTE_USER_TERMINATED);
        }
        let _ = now;
    }

    pub(crate) fn on_link_key_request(&mut self, now: u64, addr: BdAddr) {
        let key = self
            .store
            .find(addr, AddrKind::BrEdr)
            .and_then(|b| b.link_key.as_ref())
            .map(|k| k.key);
        if let Some(idx) = self.link_idx_by_addr(addr) {
            // The peer started authenticating: no need to ask it. If it
            // then stays quiet (a controller reports nothing for an
            // authentication the peer started), encryption is asked for.
            let link = &mut self.links[idx];
            link.auth_at = None;
            link.key_offered |= key.is_some();
            if key.is_some() && !link.initiated && link.encrypt_at.is_none() {
                link.encrypt_at = Some(now.saturating_add(crate::link::ENCRYPT_FALLBACK_MS));
            }
        }
        self.cmd(match key {
            Some(key) => Command::LinkKeyRequestReply { addr, key },
            None => Command::LinkKeyRequestNegativeReply { addr },
        });
    }

    pub(crate) fn on_io_capability_request(&mut self, _now: u64, addr: BdAddr) {
        let allowed = self
            .link_idx_by_addr(addr)
            .is_some_and(|i| self.links[i].pairing);
        if !allowed {
            self.refuse("pairing-off", addr);
            self.cmd(Command::IoCapabilityRequestNegativeReply {
                addr,
                reason: status::PAIRING_NOT_ALLOWED,
            });
            return;
        }
        self.note(format!("PAIR:START addr={}", red(addr)));
        self.cmd(Command::IoCapabilityRequestReply {
            addr,
            io_capability: IO_DISPLAY_YES_NO,
            oob_present: 0,
            authentication_requirements: AUTH_MITM_GENERAL_BONDING,
        });
    }

    pub(crate) fn on_io_capability_response(&mut self, addr: BdAddr, io: u8) {
        if let Some(idx) = self.link_idx_by_addr(addr) {
            self.links[idx].peer_io = Some(io);
        }
    }

    /// Just Works asks the host to confirm without a number to compare; it is
    /// answered yes for a peer that has no input and no output (a mouse) on a
    /// link allowed to pair. A peer that can show a number needs the user.
    pub(crate) fn on_user_confirmation(&mut self, addr: BdAddr) {
        let yes = self.link_idx_by_addr(addr).is_some_and(|i| {
            let link = &self.links[i];
            link.pairing && link.peer_io == Some(IO_NO_INPUT_NO_OUTPUT)
        });
        if yes {
            self.cmd(Command::UserConfirmationRequestReply { addr });
        } else {
            self.refuse("confirmation", addr);
            self.cmd(Command::UserConfirmationRequestNegativeReply { addr });
        }
    }

    /// The host cannot type a passkey (DisplayYesNo): refuse the request.
    pub(crate) fn on_user_passkey_request(&mut self, addr: BdAddr) {
        self.refuse("passkey-entry", addr);
        self.cmd(Command::UserPasskeyRequestNegativeReply { addr });
    }

    /// The passkey to show the user. It is held on the link for B5's
    /// interface; a harness build (`trace=1`) also traces it.
    pub(crate) fn on_user_passkey_notification(&mut self, addr: BdAddr, passkey: u32) {
        if let Some(idx) = self.link_idx_by_addr(addr) {
            self.links[idx].passkey = Some(passkey);
            if self.cfg.trace_reports {
                self.note(format!(
                    "PAIR:PASSKEY value={passkey:06} addr={}",
                    red(addr)
                ));
            }
        }
    }

    pub(crate) fn on_simple_pairing_complete(&mut self, code: u8, addr: BdAddr) {
        if code != status::SUCCESS {
            self.note(format!("PAIR:FAIL addr={} status={code:#04x}", red(addr)));
        }
    }

    /// The controller made a link key. It becomes a bond only on a link that
    /// may pair, and only for the key types decision 6 accepts.
    pub(crate) fn on_link_key_notification(&mut self, addr: BdAddr, key: [u8; 16], kind: u8) {
        let Some(idx) = self.link_idx_by_addr(addr) else {
            return;
        };
        let handle = self.links[idx].handle;
        if kind == key_type::CHANGED_COMBINATION {
            // A key refreshed on a bonded link: ours stays (the peer will
            // ask for it next time and we have no way to derive this one).
            return;
        }
        if !key_type_accepted(kind) {
            self.refuse("key-type", addr);
            self.disconnect(handle, status::PAIRING_NOT_ALLOWED);
            return;
        }
        if !self.links[idx].pairing {
            self.refuse("pairing-off", addr);
            self.disconnect(handle, status::PAIRING_NOT_ALLOWED);
            return;
        }
        let authenticated = key_authenticated(kind);
        let bond = Bond {
            addr,
            kind: AddrKind::BrEdr,
            authenticated,
            secure_connections: kind >= key_type::UNAUTHENTICATED_P256,
            key_size: SSP_KEY_SIZE,
            link_key: Some(LinkKey {
                key,
                key_type: kind,
            }),
            ltk: None,
            irk: None,
            class_of_device: 0,
            // Provisional: the SDP record decides what the device is.
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
        match self.store.upsert(bond) {
            Ok(()) => {
                self.links[idx].fresh_bond = true;
                self.save_bonds();
            }
            Err(error) => {
                self.note(format!(
                    "PAIR:FAIL addr={} reason=store-{error:?}",
                    red(addr)
                ));
                self.disconnect(handle, status::PAIRING_NOT_ALLOWED);
            }
        }
    }
}
