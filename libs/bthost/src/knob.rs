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

/// Why a link is refused at the key-size gate.
#[derive(Clone, Copy)]
enum Refusal {
    /// The controller reported a key shorter than a HID link may use.
    Short(u8),
    /// The controller cannot say (command unknown or refused, bad answer).
    Unsupported,
}

impl Host {
    /// The controller answered `Read_Encryption_Key_Size`.
    pub(crate) fn on_key_size(&mut self, now: u64, return_params: &[u8]) {
        let Ok(answer) = ret::parse_encryption_key_size(return_params) else {
            self.stats.malformed += 1;
            return self.key_size_unavailable(status::SUCCESS);
        };
        let handle = answer.handle;
        if answer.status != status::SUCCESS {
            // Whatever handle it names (a failed command may not know it),
            // no link waiting for a size is shown safe. An answer about a
            // link that is already gone concerns none of them.
            let gone =
                answer.status == status::UNKNOWN_CONNECTION && self.link_idx(handle).is_none();
            if !gone {
                self.key_size_unavailable(answer.status);
            }
            return;
        }
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let recheck = core::mem::take(&mut self.links[idx].key_size_recheck);
        if !self.links[idx].key_size_pending && !recheck {
            // Not asked for, or already answered: nothing to decide.
            return;
        }
        self.links[idx].key_size_pending = false;
        let size = answer.key_size;
        if size < MIN_HID_KEY_SIZE {
            return self.refuse_key_size(handle, Refusal::Short(size));
        }
        if recheck {
            // A refreshed key of full length: the link goes on untouched.
            return;
        }
        self.record_key_size(handle, size);
        self.l2
            .set_allowed(handle, &[bthid::PSM_CONTROL, bthid::PSM_INTERRUPT]);
        // What the peer sent ahead of the answer is admissible now.
        self.replay_deferred(now, handle);
        self.after_encrypted(now, handle);
    }

    /// The command was refused or its answer unusable (`code` is the status
    /// it came with), so no link waiting for a key size can be shown to be
    /// safe: they are all dropped. A controller that does not know the
    /// command is remembered, so the pager stops asking for reconnects that
    /// would all be refused the same way.
    pub(crate) fn key_size_unavailable(&mut self, code: u8) {
        if code == status::UNKNOWN_COMMAND {
            self.key_size_unsupported = true;
        }
        let waiting: Vec<u16> = self
            .links
            .iter()
            .filter(|l| l.key_size_pending || l.key_size_recheck)
            .map(|l| l.handle)
            .collect();
        for handle in waiting {
            if let Some(idx) = self.link_idx(handle) {
                self.links[idx].key_size_pending = false;
                self.links[idx].key_size_recheck = false;
            }
            self.refuse_key_size(handle, Refusal::Unsupported);
        }
    }

    /// Refuse a link for its key size. A bond made on this very link is
    /// dropped with it: its key was negotiated down, so it protects nothing.
    fn refuse_key_size(&mut self, handle: u16, why: Refusal) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let (addr, fresh) = (self.links[idx].addr, self.links[idx].fresh_bond);
        self.stats.refused += 1;
        match why {
            Refusal::Short(octets) => self.note(format!(
                "REFUSE reason=key-size addr={} octets={octets}",
                red(addr)
            )),
            Refusal::Unsupported => self.note(format!(
                "REFUSE reason=key-size-unsupported addr={}",
                red(addr)
            )),
        }
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
