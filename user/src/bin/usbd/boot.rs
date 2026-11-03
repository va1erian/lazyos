//! The boot scan, run inside the main loop (docs/bluetooth-plan.md B6).
//!
//! The ports used to be scanned in one go before the loop started, so a
//! Bluetooth controller bound on the last port waited for every slower device
//! before `btd` was answered, and `READY` came after all of them. Now the
//! loop takes one root port per pass: the controller's Messenger name is
//! registered and `btd`'s requests are served between two devices, and
//! `init` is told (`Ready`) the moment the name exists, or when the scan ends
//! without one. The scan is paced by the work itself (a port's debounce and
//! reset), never by a timer.

use user::messenger::services;
use user::sys;
use user::trace::{self, Channel};

use super::bus::Controller;
use super::Settings;

pub(super) struct Scan {
    done: bool,
    /// `init` has been told `btd` can look for the controller.
    announced: bool,
}

impl Scan {
    /// Switch the ports of every controller on; the ports are looked at by
    /// [`Scan::step`].
    pub(super) fn begin(controllers: &mut [Controller]) -> Scan {
        for controller in controllers.iter_mut() {
            controller.scan_begin();
        }
        Scan {
            done: false,
            announced: false,
        }
    }

    /// One pass: announce a served controller, then look at the next root
    /// port, or finish the scan; whether it did anything (the loop must not
    /// park while ports remain).
    pub(super) fn step(
        &mut self,
        controllers: &mut [Controller],
        settings: &Settings,
        bt_served: bool,
    ) -> bool {
        if bt_served && !self.announced {
            self.announce("bt-served");
        }
        if self.done {
            return false;
        }
        match controllers.iter_mut().find(|c| c.scan_pending()) {
            Some(controller) => {
                controller.scan_step(settings);
            }
            None => self.finish(controllers),
        }
        true
    }

    /// Every port was looked at: the kernel's late `/home` mount stops
    /// waiting for a stick once every stick present at boot was seen.
    fn finish(&mut self, controllers: &[Controller]) {
        self.done = true;
        let _ = sys::storage_scanned();
        let devices: usize = controllers.iter().map(Controller::devices).sum();
        trace::line!(
            Channel::Usbd,
            "READY devices={devices} controllers={}",
            controllers.len()
        );
        trace::span(Channel::Usbd, "scan-done");
        if !self.announced {
            self.announce("scan-done");
        }
    }

    /// `init.Ready`: `btd` starts and finds the name, or the lack of one.
    fn announce(&mut self, why: &str) {
        self.announced = true;
        services::init::notify_ready();
        trace::span(Channel::Usbd, &alloc::format!("ready-sent-{why}"));
    }
}
