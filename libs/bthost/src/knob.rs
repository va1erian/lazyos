//! The encryption key size of a BR/EDR link (CVE-2019-9506, KNOB).
//!
//! A peer can talk the link down to a key of one octet, which a bystander
//! brute-forces in seconds. Secure Simple Pairing keys are 16 octets, so a
//! HID link whose controller reports less is refused and dropped before any
//! HID channel opens. The answer to `Read_Encryption_Key_Size` is the gate:
//! `conn.rs` sends the command after a good `Encryption_Change` and holds the
//! peer's early data (`defer_acl`) until this module has the answer.

use alloc::format;
use alloc::vec::Vec;

use btbond::AddrKind;
use bthci::opcode::status;
use bthci::ret;

use crate::host::{red, Host};
use crate::link::MIN_HID_KEY_SIZE;

impl Host {
    /// The controller answered `Read_Encryption_Key_Size`.
    pub(crate) fn on_key_size(&mut self, now: u64, return_params: &[u8]) {
        let Ok(answer) = ret::parse_encryption_key_size(return_params) else {
            self.stats.malformed += 1;
            return self.key_size_unavailable();
        };
        let handle = answer.handle;
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if !self.links[idx].key_size_pending {
            // Not asked for, or already answered: nothing to decide.
            return;
        }
        self.links[idx].key_size_pending = false;
        let size = answer.key_size;
        let usable = answer.status == status::SUCCESS && size >= MIN_HID_KEY_SIZE;
        if answer.status == status::SUCCESS {
            self.record_key_size(handle, size);
        }
        if !usable {
            return self.refuse_key_size(handle, size);
        }
        self.l2
            .set_allowed(handle, &[bthid::PSM_CONTROL, bthid::PSM_INTERRUPT]);
        // What the peer sent ahead of the answer is admissible now.
        self.replay_deferred(now, handle);
        self.after_encrypted(now, handle);
    }

    /// The command was refused or its answer unreadable, so no link waiting
    /// for a key size can be shown to be safe: they are all dropped.
    pub(crate) fn key_size_unavailable(&mut self) {
        let waiting: Vec<u16> = self
            .links
            .iter()
            .filter(|l| l.key_size_pending)
            .map(|l| l.handle)
            .collect();
        for handle in waiting {
            if let Some(idx) = self.link_idx(handle) {
                self.links[idx].key_size_pending = false;
            }
            self.refuse_key_size(handle, 0);
        }
    }

    /// Refuse a link for its key size. A bond made on this very link is
    /// dropped with it: its key was negotiated down, so it protects nothing.
    fn refuse_key_size(&mut self, handle: u16, size: u8) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let (addr, fresh) = (self.links[idx].addr, self.links[idx].fresh_bond);
        self.refuse("key-size", addr);
        self.note(format!("KEYSIZE:SHORT addr={} octets={size}", red(addr)));
        if fresh && self.store.remove(addr, AddrKind::BrEdr) {
            self.save_bonds();
            self.links[idx].fresh_bond = false;
        }
        self.ctl_failed(addr, crate::ctl_types::FailReason::AuthFailed);
        self.release_hid(handle);
        self.disconnect(handle, status::AUTHENTICATION_FAILURE);
    }

    /// Keep the key size the controller reported with the bond made on this
    /// link, so the file says how strong the key is.
    fn record_key_size(&mut self, handle: u16, size: u8) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if !self.links[idx].fresh_bond {
            return;
        }
        let addr = self.links[idx].addr;
        let Some(mut bond) = self.store.find(addr, AddrKind::BrEdr).cloned() else {
            return;
        };
        if bond.key_size != size {
            bond.key_size = size;
            if self.store.upsert(bond).is_ok() {
                self.save_bonds();
            }
        }
    }
}
