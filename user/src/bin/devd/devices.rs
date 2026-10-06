//! What `devd` knows of each device: the kernel's inventory row, the manifest
//! entry it matched, and the state it is in, derived from `init`'s answers
//! and from who the inventory says holds the claim.

use alloc::string::String;
use alloc::vec::Vec;

use devinspect::Device;
use devmatch::{Entry, Function};
use user::messenger::devd::DeviceState;

/// The inventory's "nobody holds it" owner, as the wire reports it.
pub(super) const NO_OWNER: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum State {
    /// No manifest entry: the kernel drives it, or nobody does.
    Unmatched,
    /// `init` started (or was already running) its driver; not claimed yet.
    Starting,
    Claimed,
    /// Its driver held it and let it go (the driver exited or released it).
    Released,
    /// Its driver row already drives another device.
    Busy,
    /// `init` refused to start its driver.
    Failed,
    /// The manifest knows a driver for it, but this image does not ship that
    /// driver (`init` has no such row).
    NoDriver,
}

impl State {
    pub(super) fn label(self) -> &'static str {
        match self {
            State::Unmatched => "unmatched",
            State::Starting => "starting",
            State::Claimed => "claimed",
            State::Released => "released",
            State::Busy => "busy",
            State::Failed => "failed",
            State::NoDriver => "nodriver",
        }
    }
}

/// One device.
pub(super) struct Tracked {
    pub(super) device: Device,
    pub(super) entry: Option<&'static Entry>,
    pub(super) state: State,
    pub(super) pid: u64,
    /// What was last published, so only changes go out.
    pub(super) published: Option<DeviceState>,
    /// What was last logged (a publish may fail and be retried; the log line
    /// is written once per change).
    pub(super) logged: Option<DeviceState>,
    /// Whether a failed publish was reported already.
    pub(super) publish_failed: bool,
}

impl Tracked {
    pub(super) fn function(&self) -> Function {
        Function {
            id: self.device.id,
            vendor: self.device.vendor,
            device: self.device.device,
            class: self.device.class,
            subclass: self.device.subclass,
        }
    }

    /// The wire record.
    pub(super) fn record(&self) -> DeviceState {
        DeviceState {
            id: u64::from(self.device.id),
            vendor: u32::from(self.device.vendor),
            device: u32::from(self.device.device),
            class: String::from(self.device.class_name()),
            driver: String::from(self.entry.map_or("", |entry| entry.driver)),
            model: String::from(self.entry.map_or("", |entry| entry.model)),
            state: String::from(self.state.label()),
            owner: self.device.owner.unwrap_or(NO_OWNER),
            pid: self.pid,
        }
    }

    /// Take a fresh inventory row; returns whether the state changed.
    pub(super) fn refresh(&mut self, row: Device) -> bool {
        let before = self.state;
        let held = row.owner.is_some();
        self.device = row;
        self.state = match (self.state, held) {
            (State::Unmatched, _) => State::Unmatched,
            (State::NoDriver, _) => State::NoDriver,
            (_, true) => State::Claimed,
            (State::Claimed, false) => State::Released,
            (state, false) => state,
        };
        self.state != before
    }
}

/// Track every inventory row, matched against the manifest.
pub(super) fn track(rows: Vec<Device>) -> Vec<Tracked> {
    rows.into_iter()
        .map(|device| {
            let mut tracked = Tracked {
                device,
                entry: None,
                state: State::Unmatched,
                pid: 0,
                published: None,
                logged: None,
                publish_failed: false,
            };
            tracked.entry = devmatch::entry_for(&tracked.function());
            if tracked.entry.is_some() {
                tracked.state = if device.owner.is_some() {
                    State::Claimed
                } else {
                    State::Starting
                };
            }
            tracked
        })
        .collect()
}
