//! ACL links: who is admitted, authentication, encryption, and the end of a
//! link. Nothing becomes a typing device here; a link only earns the right to
//! open HID channels (`chan.rs`) by being a bonded device on an authenticated
//! and encrypted link (docs/bluetooth-plan.md decision 6).

use alloc::format;

use btbond::AddrKind;
use bthci::opcode::status;
use bthci::{BdAddr, Boundary, Command};
use btl2cap::LinkKind;

use crate::config::PairPolicy;
use crate::host::{red, Host};
use crate::link::{
    Link, AUTH_GRACE_MS, DEFER_MS, ENCRYPT_GRACE_MS, MAX_DEFERRED, MAX_DEFERRED_BYTES, MAX_LINKS,
};

/// `Connection_Request` / `Connection_Complete` link type of an ACL link.
const LINK_ACL: u8 = 1;

impl Host {
    pub(crate) fn is_bonded(&self, addr: BdAddr) -> bool {
        self.store.find(addr, AddrKind::BrEdr).is_some()
    }

    pub(crate) fn window_open(&self, now: u64) -> bool {
        self.window_until.is_some_and(|until| now < until)
    }

    /// Whether `addr` may pair now: the window is open and the policy names
    /// it (or everyone).
    pub(crate) fn pairing_allowed(&self, now: u64, addr: BdAddr) -> bool {
        self.window_open(now)
            && match &self.cfg.pair {
                PairPolicy::Off => false,
                PairPolicy::Any => true,
                PairPolicy::Targets(targets) => targets.contains(&addr),
            }
    }

    pub(crate) fn open_pairing_window(&mut self, now: u64) {
        if self.cfg.pair != PairPolicy::Off || !self.cfg.le_targets.is_empty() {
            self.window_until = Some(now.saturating_add(self.cfg.pair_window_ms));
        }
    }

    /// Refuse a device: count it and say why (the reason is a fixed word,
    /// never the peer's text).
    pub(crate) fn refuse(&mut self, reason: &str, addr: BdAddr) {
        self.stats.refused += 1;
        self.note(format!("REFUSE reason={reason} addr={}", red(addr)));
    }

    /// Hold data from a link that has been offered its key but is not yet
    /// known to be encrypted: the `Encryption_Change` may be on its way on
    /// another USB endpoint. Returns whether the fragment was held.
    pub(crate) fn defer_acl(
        &mut self,
        now: u64,
        handle: u16,
        boundary: Boundary,
        data: &[u8],
    ) -> bool {
        let Some(idx) = self.link_idx(handle) else {
            return false;
        };
        let link = &mut self.links[idx];
        if link.encrypted || !link.key_offered || link.closing {
            return false;
        }
        if link.deferred.len() >= MAX_DEFERRED
            || link.deferred_bytes + data.len() > MAX_DEFERRED_BYTES
        {
            // A peer that talks this much before it is encrypted is not
            // waiting for the host.
            self.stats.overflow += 1;
            self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
            return true;
        }
        link.deferred_bytes += data.len();
        link.deferred.push((boundary, data.to_vec()));
        link.defer_until.get_or_insert(now.saturating_add(DEFER_MS));
        true
    }

    /// Process what was held, in order: with the PSMs allowed if the link
    /// is now encrypted, refused as before if not.
    pub(crate) fn replay_deferred(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &mut self.links[idx];
        let held = core::mem::take(&mut link.deferred);
        link.deferred_bytes = 0;
        link.defer_until = None;
        self.set_replaying(handle, true);
        for (boundary, data) in held {
            let actions = self.l2.rx_acl(now, handle, boundary, &data);
            self.apply(now, actions);
        }
        self.set_replaying(handle, false);
    }

    pub(crate) fn on_connection_request(&mut self, now: u64, addr: BdAddr, link_type: u8) {
        let reject = |host: &mut Host, reason: &str, code: u8| {
            host.refuse(reason, addr);
            host.cmd(Command::RejectConnectionRequest { addr, reason: code });
        };
        if link_type != LINK_ACL {
            return reject(self, "not-acl", status::CONNECTION_REJECTED_RESOURCES);
        }
        if self.links.len() >= MAX_LINKS || self.link_idx_by_addr(addr).is_some() {
            return reject(self, "links", status::CONNECTION_REJECTED_RESOURCES);
        }
        // Decision 6: an unbonded device is admitted only to pair, and only
        // while the window says so.
        if !self.is_bonded(addr) && !self.pairing_allowed(now, addr) {
            return reject(self, "unbonded", status::CONNECTION_REJECTED_SECURITY);
        }
        // Role 0: we become the central, so we may encrypt the link.
        self.cmd(Command::AcceptConnectionRequest { addr, role: 0 });
    }

    pub(crate) fn on_connection_complete(
        &mut self,
        now: u64,
        code: u8,
        handle: u16,
        addr: BdAddr,
        link_type: u8,
    ) {
        if code != status::SUCCESS {
            self.note(format!("CONN:FAIL addr={} status={code:#04x}", red(addr)));
            self.page_status_failed(now, addr);
            return;
        }
        let pairing_page = self.page_connected(addr);
        // A controller bug or a stale handle must not corrupt the table.
        let bad = link_type != LINK_ACL
            || self.link_idx(handle).is_some()
            || self.link_idx_by_addr(addr).is_some()
            || self.links.len() >= MAX_LINKS
            || self.l2.link_up(handle, LinkKind::BrEdr).is_err();
        if bad {
            self.cmd(Command::Disconnect {
                handle,
                reason: status::CONNECTION_TERMINATED_BY_LOCAL_HOST,
            });
            return;
        }
        let initiated = pairing_page.is_some();
        let mut link = Link::new(handle, addr, now);
        link.initiated = initiated;
        link.reconnect = self.is_bonded(addr);
        link.pairing = pairing_page.unwrap_or_else(|| self.pairing_allowed(now, addr));
        let role = if initiated { "initiator" } else { "acceptor" };
        self.note(format!(
            "CONN addr={} handle={handle:#06x} role={role}",
            red(addr)
        ));
        if initiated {
            self.cmd(Command::AuthenticationRequested { handle });
        } else {
            // A device that paged us normally starts authentication itself;
            // one that stays silent is asked.
            link.auth_at = Some(now.saturating_add(AUTH_GRACE_MS));
        }
        self.links.push(link);
    }

    pub(crate) fn on_disconnection(&mut self, now: u64, code: u8, handle: u16, reason: u8) {
        if code != status::SUCCESS {
            return;
        }
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        // Whatever the device held is let go before the sources close.
        self.release_hid(handle);
        let link = self.links.remove(idx);
        let actions = self.l2.link_down(handle);
        self.apply(now, actions);
        if let Some(bufs) = &mut self.bufs {
            bufs.link_closed(handle);
        }
        self.acl_tx.retain(|(h, _, _)| *h != handle);
        self.note(format!(
            "CONN:DOWN addr={} handle={handle:#06x} reason={reason:#04x}",
            red(link.addr)
        ));
        // A device that waits to be paged is paged again, a few times.
        self.pager_relink(now, link.addr);
        if link.le.is_some() {
            self.le_link_down(now);
        }
    }

    pub(crate) fn on_authentication(&mut self, now: u64, code: u8, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        self.links[idx].auth_at = None;
        if code == status::SUCCESS {
            let link = &mut self.links[idx];
            link.authenticated = true;
            if link.initiated {
                self.cmd(Command::SetConnectionEncryption {
                    handle,
                    enable: true,
                });
            } else {
                link.encrypt_at = Some(now.saturating_add(ENCRYPT_GRACE_MS));
            }
            return;
        }
        let addr = self.links[idx].addr;
        self.note(format!("AUTH:FAIL addr={} status={code:#04x}", red(addr)));
        // A device in pairing mode that no longer knows our key: on a link
        // that may pair, forget the stale bond and let it pair afresh.
        let key_refused =
            code == status::AUTHENTICATION_FAILURE || code == status::PIN_OR_KEY_MISSING;
        let link = &self.links[idx];
        if link.pairing && !link.key_retried && key_refused && self.is_bonded(addr) {
            self.links[idx].key_retried = true;
            self.store.remove(addr, AddrKind::BrEdr);
            self.save_bonds();
            self.note(format!("PAIR:STALE addr={}", red(addr)));
            self.cmd(Command::AuthenticationRequested { handle });
        } else {
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
    }

    pub(crate) fn on_encryption(&mut self, now: u64, code: u8, handle: u16, enabled: bool) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if self.links[idx].le.is_some() {
            return self.on_le_encryption(now, code, handle, enabled);
        }
        if code != status::SUCCESS {
            let addr = self.links[idx].addr;
            self.note(format!(
                "ENCRYPT:FAIL addr={} status={code:#04x}",
                red(addr)
            ));
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
            return;
        }
        if !enabled {
            // Encryption dropped under a live HID link: nothing it says can
            // be trusted any more.
            self.links[idx].encrypted = false;
            self.l2.set_allowed(handle, &[]);
            self.release_hid(handle);
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
            return;
        }
        // Encryption on a BR/EDR link implies authentication: the keys derive
        // from the link key, so the peer has proved it holds ours. (A peer
        // that authenticates us itself gets no Authentication Complete.)
        let link = &mut self.links[idx];
        link.encrypted = true;
        link.authenticated = true;
        link.encrypt_at = None;
        let addr = link.addr;
        let keyed = self
            .store
            .find(addr, AddrKind::BrEdr)
            .is_some_and(|b| b.link_key.is_some());
        if !keyed {
            self.refuse("not-bonded", addr);
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
            return;
        }
        self.note(format!("ENCRYPT addr={}", red(addr)));
        self.l2
            .set_allowed(handle, &[bthid::PSM_CONTROL, bthid::PSM_INTERRUPT]);
        // What the peer sent ahead of the event is admissible now.
        self.replay_deferred(now, handle);
        self.after_encrypted(now, handle);
    }

    /// Deadlines of the links: a silent peer is asked to authenticate, a
    /// peer that did not encrypt is asked to, a link that never becomes a
    /// device is dropped.
    pub(crate) fn tick_links(&mut self, now: u64) {
        for idx in 0..self.links.len() {
            if self.links[idx].le.is_some() {
                let handle = self.links[idx].handle;
                self.tick_le(now, handle);
            }
            let (handle, link) = (self.links[idx].handle, &mut self.links[idx]);
            if link.auth_at.is_some_and(|t| t <= now) {
                link.auth_at = None;
                self.cmd(Command::AuthenticationRequested { handle });
                continue;
            }
            if link.encrypt_at.is_some_and(|t| t <= now) {
                link.encrypt_at = None;
                if !link.encrypted {
                    self.cmd(Command::SetConnectionEncryption {
                        handle,
                        enable: true,
                    });
                }
                continue;
            }
            if link.defer_until.is_some_and(|t| t <= now) {
                // No encryption came: the data is processed unencrypted.
                self.replay_deferred(now, handle);
                continue;
            }
            if link.open_at.is_some_and(|t| t <= now) {
                link.open_at = None;
                self.open_control_if_idle(now, handle);
                continue;
            }
            if link.hid.is_none() && !link.closing && link.setup_deadline <= now {
                let addr = link.addr;
                self.refuse("setup-timeout", addr);
                self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
            }
        }
    }
}
