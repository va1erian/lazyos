//! What a test does to the simulated world: touch a device, move a mouse,
//! lose a link.

use crate::ctrl::{PeerId, SimController, Task};
use crate::hid;

/// How long a reconnecting peer keeps paging the host.
const PAGE_WINDOW_MS: u64 = 10_000;

impl SimController {
    /// The user touches the device. A bonded peer that reconnects by paging
    /// starts paging the host (and keeps trying for a while, so a host that
    /// turns page scan on a moment later still gets it).
    pub fn wake(&mut self, id: PeerId, now_ms: u64) {
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

    /// Move a mouse peer by `(dx, dy)` with `buttons` held.
    pub fn move_mouse(&mut self, id: PeerId, now_ms: u64, buttons: u8, dx: i8, dy: i8) {
        self.wake(id, now_ms);
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

    /// The controller loses power and comes back (a host reboot with a bus-powered
    /// radio): the state of a fresh Reset, nothing queued, one command credit.
    pub fn power_cycle(&mut self) {
        self.reset();
        self.discard_pending_output();
        self.credits = 1;
    }

    /// The link drops without the host asking (out of range, battery out):
    /// the host sees a Disconnection Complete with `reason`.
    pub fn drop_link(&mut self, id: PeerId, now_ms: u64, reason: u8) {
        self.run_until(now_ms);
        if let Some(handle) = self.br(id).handle() {
            self.after(1, Task::Disconnected { handle, reason });
        }
    }
}
