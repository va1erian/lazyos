//! What a test does to the simulated world: touch a device, move a mouse,
//! lose a link.

use crate::ctrl::{PeerId, SimController, Task};
use crate::hid;
use crate::lepeer::without_id;

/// How long a reconnecting peer keeps paging the host.
const PAGE_WINDOW_MS: u64 = 10_000;

impl SimController {
    /// The user touches the device. A bonded peer that reconnects by paging
    /// starts paging the host (and keeps trying for a while, so a host that
    /// turns page scan on a moment later still gets it).
    pub fn wake(&mut self, id: PeerId, now_ms: u64) {
        if self.is_le(id) {
            return self.le_wake(id, now_ms);
        }
        self.run_until(now_ms);
        let clock = self.clock;
        let p = self.br_at(id.0);
        let reconnects = p.link_key.is_some()
            && !p.unplugged
            && p.reconnect == crate::ReconnectStyle::PeerInitiates
            && !p.is_connected()
            && p.paging_until.is_none();
        if reconnects {
            p.paging_until = Some(clock + PAGE_WINDOW_MS);
            self.after(0, Task::PeerPage { peer: id.0 });
        }
    }

    /// Press and release a key (HID usage id) on a keyboard peer.
    pub fn press_key(&mut self, id: PeerId, now_ms: u64, usage: u8) {
        self.wake(id, now_ms);
        if self.is_le(id) {
            for u in [usage, 0] {
                let (report, boot) = (
                    hid::keyboard_report(0, u, false),
                    hid::keyboard_report(0, u, true),
                );
                self.le_send(id, without_id(&report).to_vec(), boot);
            }
            return;
        }
        let p = self.br_at(id.0);
        let boot = p.boot_protocol;
        self.send_reports(
            id,
            vec![
                hid::keyboard_report(0, usage, boot),
                hid::keyboard_report(0, 0, boot),
            ],
        );
    }

    /// Send one keyboard report with `usage` held (0: everything released)
    /// and keep it held: the caller releases it (or the link drops).
    pub fn key_report(&mut self, id: PeerId, now_ms: u64, usage: u8) {
        self.wake(id, now_ms);
        if self.is_le(id) {
            let (report, boot) = (
                hid::keyboard_report(0, usage, false),
                hid::keyboard_report(0, usage, true),
            );
            return self.le_send(id, without_id(&report).to_vec(), boot);
        }
        let boot = self.br_at(id.0).boot_protocol;
        self.send_reports(id, vec![hid::keyboard_report(0, usage, boot)]);
    }

    /// Move a mouse peer by `(dx, dy)` with `buttons` held.
    pub fn move_mouse(&mut self, id: PeerId, now_ms: u64, buttons: u8, dx: i8, dy: i8) {
        self.wake(id, now_ms);
        if self.is_le(id) {
            let report = hid::mouse_report(buttons, dx, dy, 0, false);
            let boot = hid::mouse_report(buttons, dx, dy, 0, true);
            return self.le_send(id, without_id(&report).to_vec(), boot);
        }
        let boot = self.br_at(id.0).boot_protocol;
        self.send_reports(id, vec![hid::mouse_report(buttons, dx, dy, 0, boot)]);
    }

    /// Queue arbitrary input report bytes (after the `0xA1` header).
    pub fn send_reports(&mut self, id: PeerId, reports: Vec<Vec<u8>>) {
        let p = self.br_at(id.0);
        // A device nobody is bonded to has nobody to tell.
        if !p.is_connected() && p.link_key.is_none() {
            return;
        }
        reports.into_iter().for_each(|r| p.queue_report(r));
        if let Some(handle) = p.handle() {
            let pdus = p.flush_reports();
            self.peer_sends(handle, pdus);
        }
    }

    /// Send `message` on the HID control channel (a device-initiated
    /// operation: `HID_CONTROL`, a handshake). Nothing happens while the
    /// channel is not open.
    pub fn send_control(&mut self, id: PeerId, now_ms: u64, message: &[u8]) {
        self.run_until(now_ms);
        let p = self.br_at(id.0);
        if let Some(handle) = p.handle() {
            let pdus = p.control_pdus(message);
            self.peer_sends(handle, pdus);
        }
    }

    /// The device unplugs its virtual cable (`HID_CONTROL`): it is leaving
    /// the host for good.
    pub fn unplug(&mut self, id: PeerId, now_ms: u64) {
        self.send_control(
            id,
            now_ms,
            &[hid::hidp::HID_CONTROL | hid::hidp::VIRTUAL_CABLE_UNPLUG],
        );
    }

    /// The controller loses power and comes back (a host reboot with a bus-powered
    /// radio): the state of a fresh Reset, nothing queued, one command credit.
    pub fn power_cycle(&mut self) {
        self.reset();
        self.discard_pending_output();
        self.credits = 1;
    }

    /// Change the key size `Read_Encryption_Key_Size` reports from now on
    /// (a peer that negotiates the link key down, CVE-2019-9506).
    pub fn set_br_key_size(&mut self, size: u8) {
        self.cfg.br_key_size = size;
    }

    /// The key size answer comes `ms` later than other command completions.
    pub fn set_br_key_size_delay(&mut self, ms: u64) {
        self.cfg.br_key_size_delay_ms = ms;
    }

    /// `Read_Encryption_Key_Size` becomes an unknown command (or not).
    pub fn set_br_key_size_unknown(&mut self, unknown: bool) {
        self.cfg.br_key_size_unknown = unknown;
    }

    /// The link drops without the host asking (out of range, battery out):
    /// the host sees a Disconnection Complete with `reason`.
    pub fn drop_link(&mut self, id: PeerId, now_ms: u64, reason: u8) {
        self.run_until(now_ms);
        if let Some(handle) = self.link_handle(id) {
            self.after(1, Task::Disconnected { handle, reason });
        }
    }

    /// Queue an input report on an LE device (report protocol bytes without
    /// the id, and the boot protocol bytes) and send it if the host is
    /// subscribed; otherwise it waits, as a keystroke does on a real device.
    fn le_send(&mut self, id: PeerId, report: Vec<u8>, boot: Vec<u8>) {
        let p = self.le_at(id.0);
        p.queue_report(report, boot);
        if let Some(handle) = p.handle() {
            let pdus = p.flush();
            self.peer_sends(handle, pdus);
        }
    }

    /// Whether the device would deliver a report now: BR/EDR with both HID
    /// channels open, LE encrypted and subscribed.
    pub fn input_ready(&self, id: PeerId) -> bool {
        if self.is_le(id) {
            let p = self.le(id);
            p.is_encrypted() && p.subscribed()
        } else {
            self.br(id).hid_channels_open() == (true, true)
        }
    }

    /// The device's stored key and whether it is authenticated (BR/EDR:
    /// link key and key type; LE: LTK and 1 for an authenticated key).
    pub fn bond_of(&self, id: PeerId) -> Option<([u8; 16], u8)> {
        if self.is_le(id) {
            self.le(id).bond
        } else {
            self.br(id).link_key
        }
    }

    pub fn set_bond(&mut self, id: PeerId, bond: Option<([u8; 16], u8)>) {
        if self.is_le(id) {
            self.le_at(id.0).bond = bond;
        } else {
            self.br_at(id.0).link_key = bond;
        }
    }

    pub fn peer_addr(&self, id: PeerId) -> bthci::BdAddr {
        self.peers[id.0].addr()
    }

    /// Put an unbonded device in pairing mode (the user holds the button).
    pub fn enter_pairing_mode(&mut self, id: PeerId, now_ms: u64) {
        if self.is_le(id) {
            self.le_enter_pairing_mode(id, now_ms);
        } else {
            self.br_at(id.0).set_pairing_mode(true);
        }
    }
}
