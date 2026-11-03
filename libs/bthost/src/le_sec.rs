//! LE security: pairing with `btsmp`, link encryption with the new or the
//! stored key, and the controller asking for a key.
//!
//! Decision 6: nothing is acted on until the link is encrypted with the
//! bond's key. A pairing derives its key in SMP, starts encryption with it,
//! and only then believes the key distribution that follows. A reconnect
//! starts encryption with the stored LTK before it asks the device anything.
//! Data that races the `Encryption_Change` (a different USB endpoint) is held
//! by `conn.rs`'s deferral and replayed once the event has been handled, so
//! SMP always sees `Encryption { ok }` before the PDUs that depend on it.

use alloc::format;
use alloc::vec::Vec;

use btbond::AddrKind;
use bthci::opcode::{self, status};
use bthci::{BdAddr, Command};
use btsmp::pdu::AddrType;
use btsmp::{Central, Config as SmpConfig, DeviceAddr, Input, Notice, Step};
use lazyos_crypto::rng::Entropy;

use crate::host::{red, Host};
use crate::le_link::{Fresh, PendingKey};

/// The L2CAP fixed channel of SMP.
const CID_SMP: u16 = 6;
/// The company id `btsim` reports in Read_Local_Version: 0xFFFF, which the
/// Bluetooth SIG reserves for internal testing, so no shipping radio has it.
const SIMULATOR_MANUFACTURER: u16 = 0xFFFF;

impl Host {
    /// Mix `bytes` into the pairing entropy. The shell calls this once at
    /// start with what the kernel's `random` gave it; a host that was never
    /// seeded refuses to pair rather than make guessable keys.
    pub fn seed_entropy(&mut self, bytes: &[u8]) {
        self.entropy.seed(bytes);
    }

    fn pairing_entropy(&mut self) -> Option<Entropy> {
        if !self.entropy.is_seeded() {
            return None;
        }
        let mut seed = [0u8; 32];
        self.entropy.fill(&mut seed);
        let mut fresh = Entropy::new();
        fresh.seed(&seed);
        Some(fresh)
    }

    /// The controller's own address, once `Read_BD_ADDR` has answered.
    fn local_address(&self) -> Option<BdAddr> {
        self.init.info().bd_addr
    }

    /// Start SMP on an unbonded link that may pair.
    pub(crate) fn le_begin_pairing(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let Some(local) = self.local_address() else {
            if let Some(le) = &mut self.links[idx].le {
                le.waits_addr = true;
            }
            return;
        };
        let Some(entropy) = self.pairing_entropy() else {
            let addr = self.links[idx].addr;
            self.note(format!("PAIR:FAIL addr={} reason=no-entropy", red(addr)));
            return self.disconnect(handle, status::PAIRING_NOT_ALLOWED);
        };
        let link = &mut self.links[idx];
        let Some(le) = link.le.as_mut() else { return };
        le.waits_addr = false;
        let kind = |t: u8| {
            if t == 0 {
                AddrType::Public
            } else {
                AddrType::Random
            }
        };
        let cfg = SmpConfig::new(
            DeviceAddr {
                kind: AddrType::Public,
                addr: local.0,
            },
            DeviceAddr {
                kind: kind(le.addr_type),
                addr: le.conn_addr.0,
            },
        );
        let mut central = Central::new(cfg, entropy);
        let step = central.step(now, Input::Start);
        le.smp = Some(central);
        let addr = link.addr;
        self.note(format!("PAIR:START addr={}", red(addr)));
        self.smp_apply(now, handle, step);
    }

    /// The controller's address arrived: links that waited can pair.
    pub(crate) fn le_address_ready(&mut self, now: u64) {
        let waiting: Vec<u16> = self
            .links
            .iter()
            .filter(|l| l.le.as_ref().is_some_and(|le| le.waits_addr))
            .map(|l| l.handle)
            .collect();
        for handle in waiting {
            self.le_begin_pairing(now, handle);
        }
    }

    /// Carry out what a `btsmp` step asks.
    pub(crate) fn smp_apply(&mut self, now: u64, handle: u16, step: Step) {
        for pdu in &step.send {
            self.send_on(handle, CID_SMP, pdu);
        }
        let mut declined = false;
        for notice in step.notices {
            match notice {
                Notice::DisplayPasskey(passkey) => self.show_passkey(handle, passkey),
                // A number to compare needs a person; B5's interface.
                Notice::ConfirmNumber(_) => declined = true,
                Notice::StartEncryption {
                    ltk,
                    key_size,
                    authenticated,
                } => self.smp_start_encryption(now, handle, *ltk.expose(), key_size, authenticated),
            }
        }
        if let Some(idx) = self.link_idx(handle) {
            if let Some(le) = self.links[idx].le.as_mut() {
                le.smp_deadline = step.deadline_ms;
            }
        }
        match step.outcome {
            Some(Ok(bond)) => self.on_smp_done(now, handle, bond),
            Some(Err(error)) => self.on_smp_failed(handle, error),
            None if declined => {
                let addr = self.link_idx(handle).map(|i| self.links[i].addr);
                if let Some(addr) = addr {
                    self.refuse("confirmation", addr);
                }
                self.smp_input(now, handle, Input::UserConfirm(false));
            }
            None => {}
        }
    }

    pub(crate) fn smp_input(&mut self, now: u64, handle: u16, input: Input<'_>) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let Some(smp) = self.links[idx].le.as_mut().and_then(|le| le.smp.as_mut()) else {
            return;
        };
        let step = smp.step(now, input);
        self.smp_apply(now, handle, step);
    }

    fn show_passkey(&mut self, handle: u16, passkey: u32) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        self.links[idx].passkey = Some(passkey);
        if self.cfg.trace_reports {
            self.note(format!(
                "PAIR:PASSKEY value={passkey:06} addr={}",
                red(addr)
            ));
            if self.simulated_controller() {
                self.harness_show_passkey(passkey);
            }
            // Otherwise the version may not be read yet (it is the last
            // command of bring-up, and LE pairing starts before it): the
            // link keeps the passkey and `harness_flush_passkeys` sends it
            // once the version names the simulator.
        }
    }

    /// The controller is `btsim`, which reports the SIG's testing company id.
    /// Only then may the harness opcode go out: on a real radio a
    /// harness-built image must never send a vendor opcode that means
    /// something else there, nor the passkey.
    fn simulated_controller(&self) -> bool {
        self.init
            .info()
            .version
            .is_some_and(|v| v.manufacturer == SIMULATOR_MANUFACTURER)
    }

    /// The harness plays the user: tell the simulated controller what the
    /// screen shows (`bthci::opcode::HARNESS_SHOW_PASSKEY`).
    fn harness_show_passkey(&mut self, passkey: u32) {
        self.cmd(Command::Unknown {
            opcode: opcode::HARNESS_SHOW_PASSKEY,
            params: passkey.to_le_bytes().to_vec(),
        });
    }

    /// Called once bring-up has read the version: send the passkeys of LE
    /// pairings that started before it was known.
    pub(crate) fn harness_flush_passkeys(&mut self) {
        if !self.cfg.trace_reports || !self.simulated_controller() {
            return;
        }
        let pending: Vec<u32> = self
            .links
            .iter()
            .filter(|l| {
                l.le.as_ref()
                    .and_then(|le| le.smp.as_ref())
                    .is_some_and(|smp| smp.is_active())
            })
            .filter_map(|l| l.passkey)
            .collect();
        for passkey in pending {
            self.harness_show_passkey(passkey);
        }
    }

    fn smp_start_encryption(
        &mut self,
        now: u64,
        handle: u16,
        ltk: [u8; 16],
        key_size: u8,
        authenticated: bool,
    ) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        if let Some(le) = self.links[idx].le.as_mut() {
            le.pending_key = Some(PendingKey {
                ltk,
                key_size,
                authenticated,
            });
        }
        // A Secure Connections key is issued with EDIV and Rand zero.
        self.le_encrypt_with(now, handle, ltk, 0, 0);
    }

    /// Ask the controller to encrypt the link with `ltk`. From here, data
    /// from the peer waits for the `Encryption_Change`.
    pub(crate) fn le_encrypt_with(
        &mut self,
        now: u64,
        handle: u16,
        ltk: [u8; 16],
        ediv: u16,
        rand: u64,
    ) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let link = &mut self.links[idx];
        link.key_offered = true;
        if let Some(le) = link.le.as_mut() {
            le.enc_deadline = Some(now.saturating_add(crate::le::ENCRYPT_MS));
        }
        self.cmd(Command::LeStartEncryption {
            handle,
            random: rand,
            ediv,
            ltk,
        });
    }

    /// SMP finished: the keys are made and the link is encrypted with them.
    /// The bond is stored only after discovery shows what the device is.
    fn on_smp_done(&mut self, now: u64, handle: u16, bond: btsmp::Bond) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let conn = self.links[idx].le.as_ref().map(|le| le.conn_addr);
        let identity = bond.peer_identity.unwrap_or(bond.peer_address);
        let kind = match identity.kind {
            AddrType::Public => AddrKind::LePublic,
            AddrType::Random => AddrKind::LeRandom,
        };
        let addr = BdAddr(identity.addr);
        // A device on a private address that gave no identity can never be
        // recognised again: there is nothing to bond.
        let unidentifiable = bond.peer_identity.is_none()
            && conn.is_some_and(|c| c.0[5] & 0xC0 != 0xC0 && kind == AddrKind::LeRandom);
        if unidentifiable || self.link_idx_by_addr(addr).is_some_and(|i| i != idx) {
            let old = self.links[idx].addr;
            self.note(format!("PAIR:FAIL addr={} reason=no-identity", red(old)));
            self.le_deny(old);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
        let link = &mut self.links[idx];
        link.addr = addr;
        link.authenticated = bond.authenticated;
        let key = PendingKey {
            ltk: *bond.ltk.expose(),
            key_size: bond.key_size,
            authenticated: bond.authenticated,
        };
        if let Some(le) = link.le.as_mut() {
            le.smp = None;
            le.smp_deadline = None;
            le.pending_key = None;
            le.fresh = Some(Fresh {
                key,
                irk: bond.peer_irk.as_ref().map(|k| *k.expose()),
                kind,
            });
        }
        self.le_discover(now, handle);
    }

    fn on_smp_failed(&mut self, handle: u16, error: btsmp::Error) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        self.note(format!(
            "PAIR:FAIL addr={} reason=smp-{}",
            red(addr),
            smp_reason(&error)
        ));
        if let Some(le) = self.links[idx].le.as_mut() {
            le.smp = None;
            le.smp_deadline = None;
        }
        self.le_deny(addr);
        self.disconnect(handle, status::AUTHENTICATION_FAILURE);
    }

    /// `Encryption_Change` on an LE link.
    pub(crate) fn on_le_encryption(&mut self, now: u64, code: u8, handle: u16, enabled: bool) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        if let Some(le) = self.links[idx].le.as_mut() {
            le.enc_deadline = None;
        }
        if code != status::SUCCESS || !enabled {
            return self.le_encryption_failed(now, handle, code, enabled);
        }
        let link = &mut self.links[idx];
        link.encrypted = true;
        let bonded = link
            .le
            .as_ref()
            .is_some_and(|le| le.smp.is_none() && le.fresh.is_none());
        self.note(format!("ENCRYPT addr={}", red(addr)));
        // SMP first: the key distribution that follows is believed only
        // once SMP has seen the link encrypted.
        self.smp_input(now, handle, Input::Encryption { ok: true });
        self.replay_deferred(now, handle);
        if bonded {
            self.le_resume(now, handle);
        }
    }

    fn le_encryption_failed(&mut self, now: u64, handle: u16, code: u8, enabled: bool) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        self.note(format!(
            "ENCRYPT:FAIL addr={} status={code:#04x}",
            red(addr)
        ));
        if code == status::SUCCESS && !enabled {
            // Encryption dropped under a live link: nothing it says counts.
            self.links[idx].encrypted = false;
            self.release_hid(handle);
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
        if self.links[idx]
            .le
            .as_ref()
            .is_some_and(|le| le.smp.is_some())
        {
            self.smp_input(now, handle, Input::Encryption { ok: false });
            return self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
        // A device in pairing mode that no longer knows our key: forget the
        // stale bond and pair afresh on this link (as for BR/EDR).
        let link = &self.links[idx];
        let stale = link.pairing && code == status::PIN_OR_KEY_MISSING && link.reconnect;
        if stale {
            let kind = self.store.iter().find(|b| b.addr == addr).map(|b| b.kind);
            if let Some(kind) = kind {
                self.store.remove(addr, kind);
                self.save_bonds();
            }
            self.links[idx].reconnect = false;
            self.links[idx].key_offered = false;
            self.note(format!("PAIR:STALE addr={}", red(addr)));
            self.le_begin_pairing(now, handle);
        } else {
            self.disconnect(handle, status::AUTHENTICATION_FAILURE);
        }
    }

    /// The controller asks for the key of a link (the peripheral role; the
    /// host is always the central here, but a controller may still ask): the
    /// stored LTK, only if the EDIV and Rand are the ones it was issued under.
    pub(crate) fn on_ltk_request(&mut self, handle: u16, random: u64, ediv: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return self.cmd(Command::LeLongTermKeyRequestNegativeReply { handle });
        };
        let addr = self.links[idx].addr;
        let key = self
            .store
            .iter()
            .find(|b| b.addr == addr && b.kind.is_le())
            .and_then(|b| b.ltk.as_ref())
            .filter(|k| k.rand == random && k.ediv == ediv)
            .map(|k| k.key);
        self.cmd(match key {
            Some(ltk) => Command::LeLongTermKeyRequestReply { handle, ltk },
            None => Command::LeLongTermKeyRequestNegativeReply { handle },
        });
    }

    /// Deadlines of an LE link: SMP's 30 s timeout, the encryption wait, the
    /// ATT transaction timeout.
    pub(crate) fn tick_le(&mut self, now: u64, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let Some(le) = self.links[idx].le.as_mut() else {
            return;
        };
        let smp_due = le.smp_deadline.is_some_and(|t| t <= now);
        let enc_due = le.enc_deadline.is_some_and(|t| t <= now);
        let att_due = le
            .gatt
            .as_ref()
            .and_then(|g| g.deadline())
            .is_some_and(|t| t <= now);
        if smp_due {
            le.smp_deadline = None;
            self.smp_input(now, handle, Input::Tick);
        }
        if enc_due {
            self.note_enc_timeout(handle);
        }
        if att_due {
            self.le_att_tick(now, handle);
        }
    }

    fn note_enc_timeout(&mut self, handle: u16) {
        let Some(idx) = self.link_idx(handle) else {
            return;
        };
        let addr = self.links[idx].addr;
        if let Some(le) = self.links[idx].le.as_mut() {
            le.enc_deadline = None;
        }
        self.note(format!("ENCRYPT:FAIL addr={} status=timeout", red(addr)));
        self.disconnect(handle, status::CONNECTION_TERMINATED_BY_LOCAL_HOST);
    }
}

/// A fixed word for why SMP ended (never the peer's text).
fn smp_reason(error: &btsmp::Error) -> &'static str {
    use btsmp::Error::*;
    match error {
        Timeout => "timeout",
        PeerFailed(_) => "peer-failed",
        Cancelled => "cancelled",
        LegacyPairingRefused => "legacy",
        KeySizeTooSmall { .. } => "key-size",
        OobUnsupported => "oob",
        AuthenticationRequired => "authentication-required",
        ConfirmMismatch => "confirm-mismatch",
        DhKeyCheckMismatch => "dhkey-mismatch",
        UserRejected => "user-rejected",
        InvalidPublicKey | WeakPublicKey => "public-key",
        KeyDistributionMismatch => "key-distribution",
        InvalidIdentity => "identity",
        Malformed(_) => "malformed",
        UnexpectedPdu { .. } => "unexpected-pdu",
        EncryptionFailed => "encryption",
        Unseeded => "unseeded",
    }
}
