//! The rig of the firmware-phase tests: the shell in miniature. It feeds the
//! host what the simulated vendor radio sends, answers the host's file
//! requests from a map, and re-enumerates the radio the way `btd` does
//! (tell the host, start a new one with the count of re-enumerations).

use std::collections::BTreeMap;
use std::string::String;
use std::vec;
use std::vec::Vec;

use btbond::BondStore;
use btsim::{Controller, HciIn, Part, RadioFaults, SimConfig, SimController};

use super::fw_images::{intel_sfi, DDC};
use crate::{Config, Effect, Host, UsbIds};

type Files = BTreeMap<String, Vec<u8>>;

pub struct FwRig {
    pub sim: SimController,
    pub host: Host,
    pub now: u64,
    pub files: Files,
    pub ids: Option<UsbIds>,
    pub traces: Vec<String>,
    pub asked: Vec<(String, bool)>,
    pub failed: Option<String>,
    pub hosts: u32,
    /// Leave a detaching part on the bus (the test looks at the host first).
    pub hold_detach: bool,
}

pub fn ids_of(part: Part) -> UsbIds {
    let (vendor, product) = part.usb_ids();
    UsbIds { vendor, product }
}

impl FwRig {
    pub fn new(part: Part, faults: RadioFaults, files: &[(&str, Vec<u8>)]) -> FwRig {
        let mut sim = SimController::new(SimConfig::default());
        sim.set_radio(part, faults);
        let ids = Some(ids_of(part));
        FwRig {
            host: Host::new_for(Config::default(), BondStore::new(), 0, ids, 0),
            sim,
            now: 0,
            files: files
                .iter()
                .map(|(n, d)| (String::from(*n), d.clone()))
                .collect(),
            ids,
            traces: Vec::new(),
            asked: Vec::new(),
            failed: None,
            hosts: 1,
            hold_detach: false,
        }
    }

    pub fn generic() -> FwRig {
        let mut rig = FwRig::new(Part::IntelAx201, RadioFaults::default(), &[]);
        rig.sim = SimController::new(SimConfig::default());
        rig.ids = Some(UsbIds {
            vendor: 0x0a12,
            product: 0x0001,
        });
        rig.host = Host::new_for(Config::default(), BondStore::new(), 0, rig.ids, 0);
        rig
    }

    pub fn seen(&self, prefix: &str) -> bool {
        self.traces.iter().any(|t| t.starts_with(prefix))
    }

    pub fn count(&self, prefix: &str) -> usize {
        self.traces.iter().filter(|t| t.starts_with(prefix)).count()
    }

    /// Move everything one step: host to controller, controller to host.
    pub fn pump(&mut self) -> bool {
        let mut progress = false;
        while let Some(packet) = self.host.poll_command(self.now) {
            self.sim.command(self.now, &packet);
            progress = true;
        }
        while let Some(effect) = self.host.poll_effect() {
            progress = true;
            match effect {
                Effect::Trace(text) => self.traces.push(text),
                Effect::LoadFirmware { name, required } => {
                    let file = self.files.get(&name).cloned();
                    self.asked.push((name, required));
                    self.host.on_firmware_file(self.now, file);
                }
                Effect::Failed(why) => self.failed = Some(why),
                _ => {}
            }
        }
        while let Some(packet) = self.sim.poll_in(self.now) {
            progress = true;
            match packet {
                HciIn::Event(bytes) => self.host.on_event(self.now, &bytes),
                HciIn::Acl(bytes) => self.host.on_acl(self.now, &bytes),
            }
        }
        let leaving = !self.hold_detach;
        if let Some(gone_for) = self.sim.take_detach(self.now).filter(|_| leaving) {
            self.re_enumerate(gone_for);
            progress = true;
        }
        progress
    }

    /// The controller left the bus: the shell tells the host, then starts a
    /// new one when the device is back.
    pub fn re_enumerate(&mut self, gone_for: u64) {
        self.host.on_detached(self.now);
        while let Some(effect) = self.host.poll_effect() {
            if let Effect::Trace(text) = effect {
                self.traces.push(text);
            }
        }
        let carried = self.host.firmware_attempts();
        self.now += gone_for;
        self.host = Host::new_for(
            Config::default(),
            BondStore::new(),
            self.now,
            self.ids,
            carried,
        );
        self.hosts += 1;
    }

    pub fn run(&mut self, until: u64, stop: impl Fn(&FwRig) -> bool) {
        loop {
            while self.pump() {}
            if stop(self) {
                return;
            }
            let due = [self.host.next_deadline(), self.sim.next_due()]
                .into_iter()
                .flatten()
                .min();
            match due {
                Some(t) if t <= until => {
                    if t <= self.now {
                        if self.host.next_deadline().is_some_and(|d| d <= self.now) {
                            self.host.on_tick(self.now);
                        } else {
                            self.now += 1;
                        }
                    } else {
                        self.now = t;
                    }
                }
                _ => {
                    self.now = until;
                    return;
                }
            }
        }
    }

    pub fn up(&mut self) {
        self.run(120_000, |r| r.seen("CTRL:PASS") || r.failed.is_some());
    }
}

pub fn intel_files(ax211: bool) -> Vec<(&'static str, Vec<u8>)> {
    let sfi = intel_sfi(ax211);
    if ax211 {
        vec![
            ("intel/ibt-0040-0041.sfi", sfi),
            ("intel/ibt-0040-0041.ddc", DDC.to_vec()),
        ]
    } else {
        vec![
            ("intel/ibt-19-0-4.sfi", sfi),
            ("intel/ibt-19-0-4.ddc", DDC.to_vec()),
        ]
    }
}
