//! The Bluetooth seam (docs/bluetooth-plan.md B5): what the Bluetooth page
//! asks of `btd` (`os.lazy.bt.v1`), and the model the page shows.
//!
//! **The window never waits on `btd`.** Every call of [`Bluetooth`] only
//! queues a request and returns; the answers (the device list, a scan result,
//! the passkey to show, a refusal) come back later as [`BtEvent`]s through the
//! sink given to [`Bluetooth::start`], from the platform's worker threads,
//! and the window applies them as ordinary messages. On LazyOS the binary
//! implements it over Messenger (`xui_app::platform::bluetooth`);
//! [`MemBluetooth`] stands in for tests and previews.
//!
//! No call here can carry a key: the service never returns one.

use std::cell::RefCell;
use std::sync::Arc;

/// How a device is reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtTransport {
    Classic,
    LowEnergy,
}

/// What a device types with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtInput {
    Unknown,
    Keyboard,
    Pointer,
}

/// The last two octets of an address, the form a trace may carry
/// (docs/bluetooth-plan.md decision 9).
pub fn redacted(address: &str) -> String {
    let tail: Vec<&str> = address.rsplit(':').take(2).collect();
    match tail[..] {
        [b, a] => format!("xx:xx:xx:xx:{a}:{b}"),
        _ => String::from("xx"),
    }
}

/// One device in the list. `Debug` shows the redacted address: the window
/// traces its messages.
#[derive(Clone, PartialEq, Eq)]
pub struct BtDevice {
    /// `AA:BB:CC:DD:EE:FF`.
    pub address: String,
    pub name: String,
    pub transport: BtTransport,
    pub input: BtInput,
    /// dBm of the last scan result.
    pub rssi: Option<i32>,
    pub bonded: bool,
    pub connected: bool,
}

impl std::fmt::Debug for BtDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BtDevice")
            .field("address", &redacted(&self.address))
            .field("name", &self.name)
            .field("transport", &self.transport)
            .field("input", &self.input)
            .field("bonded", &self.bonded)
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

impl BtDevice {
    /// The list row: name, then what it is and its state.
    pub fn row(&self) -> String {
        let name = if self.name.is_empty() {
            match self.input {
                BtInput::Keyboard => "Keyboard",
                BtInput::Pointer => "Mouse",
                BtInput::Unknown => "Device",
            }
        } else {
            self.name.as_str()
        };
        let state = match (self.connected, self.bonded) {
            (true, _) => "connected",
            (false, true) => "paired",
            (false, false) => "found",
        };
        let how = match self.transport {
            BtTransport::Classic => "classic",
            BtTransport::LowEnergy => "low energy",
        };
        let signal = self
            .rssi
            .map_or(String::new(), |dbm| format!(", {dbm} dBm"));
        format!("{name}   ({state}, {how}{signal})")
    }
}

/// Where a pairing is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtPhase {
    Idle,
    Connecting,
    ShowPasskey,
    ConfirmNumber,
    Paired,
    Failed,
}

/// Why a pairing failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtReason {
    None,
    Timeout,
    Cancelled,
    Rejected,
    AuthFailed,
    Unsupported,
    LinkLost,
}

impl BtReason {
    /// The sentence the page shows.
    pub fn text(self) -> &'static str {
        match self {
            BtReason::None => "The pairing failed.",
            BtReason::Timeout => "The device did not answer in time.",
            BtReason::Cancelled => "The pairing was cancelled.",
            BtReason::Rejected => "The pairing was rejected, or the numbers did not match.",
            BtReason::AuthFailed => "The device could not prove who it is.",
            BtReason::Unsupported => {
                "This device cannot be paired safely (a keyboard needs a passkey)."
            }
            BtReason::LinkLost => "The connection dropped during the pairing.",
        }
    }
}

/// The pairing in progress, or the outcome of the last one. `Debug` hides
/// the passkey and the number: the window traces its messages, and a passkey
/// is for the screen alone.
#[derive(Clone, PartialEq, Eq)]
pub struct BtPairing {
    pub phase: BtPhase,
    pub address: String,
    pub passkey: u32,
    pub number: u32,
    pub reason: BtReason,
}

impl std::fmt::Debug for BtPairing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BtPairing")
            .field("phase", &self.phase)
            .field("address", &redacted(&self.address))
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl BtPairing {
    pub fn idle() -> BtPairing {
        BtPairing {
            phase: BtPhase::Idle,
            address: String::new(),
            passkey: 0,
            number: 0,
            reason: BtReason::None,
        }
    }

    /// A pairing is under way (its dialog is up).
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            BtPhase::Connecting | BtPhase::ShowPasskey | BtPhase::ConfirmNumber
        )
    }
}

/// What the service tells the page, from a worker thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtEvent {
    /// The whole picture, after a (re)connect or a refresh.
    Snapshot {
        controller: bool,
        discovering: bool,
        devices: Vec<BtDevice>,
        pairing: BtPairing,
    },
    /// A device appeared, changed (`removed` false) or went.
    Device {
        device: BtDevice,
        removed: bool,
    },
    Pairing(BtPairing),
    /// A scan started or ended.
    Discovering(bool),
    /// A call was refused or the service is not reachable: the text to show.
    Failed(String),
}

/// Delivers [`BtEvent`]s to the window, from any thread.
pub type BtSink = Arc<dyn Fn(BtEvent) + Send + Sync>;

/// The Bluetooth service as the page uses it. Every method queues a request
/// and returns at once.
pub trait Bluetooth {
    /// Start talking to the service (once; later calls are ignored) and
    /// deliver the events to `sink`. A snapshot follows.
    fn start(&self, sink: BtSink);
    /// Ask for a fresh snapshot (and reconnect if the service went away).
    fn refresh(&self);
    /// Scan for devices.
    fn scan(&self);
    fn stop_scan(&self);
    fn pair(&self, address: &str);
    /// Answer a number to compare.
    fn confirm(&self, address: &str, accept: bool);
    fn cancel(&self);
    fn forget(&self, address: &str);
}

/// Everything a [`MemBluetooth`] was asked, in order, for tests to read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtCall {
    Start,
    Refresh,
    Scan,
    StopScan,
    Pair(String),
    Confirm(String, bool),
    Cancel,
    Forget(String),
}

/// A [`Bluetooth`] that records its calls and delivers nothing by itself: a
/// test sends the events it wants as messages.
#[derive(Default)]
pub struct MemBluetooth {
    pub calls: RefCell<Vec<BtCall>>,
}

impl MemBluetooth {
    fn note(&self, call: BtCall) {
        self.calls.borrow_mut().push(call);
    }
}

impl Bluetooth for MemBluetooth {
    fn start(&self, _sink: BtSink) {
        self.note(BtCall::Start);
    }
    fn refresh(&self) {
        self.note(BtCall::Refresh);
    }
    fn scan(&self) {
        self.note(BtCall::Scan);
    }
    fn stop_scan(&self) {
        self.note(BtCall::StopScan);
    }
    fn pair(&self, address: &str) {
        self.note(BtCall::Pair(address.into()));
    }
    fn confirm(&self, address: &str, accept: bool) {
        self.note(BtCall::Confirm(address.into(), accept));
    }
    fn cancel(&self) {
        self.note(BtCall::Cancel);
    }
    fn forget(&self, address: &str) {
        self.note(BtCall::Forget(address.into()));
    }
}

/// What the page knows: the list, the scan and the pairing.
#[derive(Clone, Debug)]
pub struct BtModel {
    pub controller: bool,
    pub discovering: bool,
    pub devices: Vec<BtDevice>,
    pub pairing: BtPairing,
    /// The last refusal or outage text.
    pub problem: Option<String>,
}

impl Default for BtModel {
    fn default() -> BtModel {
        BtModel {
            controller: false,
            discovering: false,
            devices: Vec::new(),
            pairing: BtPairing::idle(),
            problem: None,
        }
    }
}

/// The most devices the page lists: the service bounds its own list, the page
/// does not trust it.
pub const MAX_DEVICES: usize = 64;

impl BtModel {
    /// Apply one event.
    pub fn apply(&mut self, event: BtEvent) {
        match event {
            BtEvent::Snapshot {
                controller,
                discovering,
                mut devices,
                pairing,
            } => {
                devices.truncate(MAX_DEVICES);
                *self = BtModel {
                    controller,
                    discovering,
                    devices,
                    pairing,
                    problem: None,
                };
            }
            BtEvent::Device { device, removed } => self.device(device, removed),
            BtEvent::Pairing(pairing) => {
                // The service ends a scan when a pairing starts.
                self.discovering &= !pairing.active();
                self.pairing = pairing;
                self.problem = None;
            }
            BtEvent::Discovering(on) => self.discovering = on,
            BtEvent::Failed(text) => self.problem = Some(text),
        }
    }

    fn device(&mut self, device: BtDevice, removed: bool) {
        let at = self
            .devices
            .iter()
            .position(|d| d.address == device.address);
        match (at, removed) {
            (Some(i), true) => {
                self.devices.remove(i);
            }
            (Some(i), false) => self.devices[i] = device,
            (None, false) if self.devices.len() < MAX_DEVICES => self.devices.push(device),
            _ => {}
        }
    }

    /// The device at list row `row`.
    pub fn at(&self, row: usize) -> Option<&BtDevice> {
        self.devices.get(row)
    }

    /// The device being paired, if it is listed.
    pub fn pairing_device(&self) -> Option<&BtDevice> {
        self.devices
            .iter()
            .find(|d| d.address == self.pairing.address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(address: &str, name: &str) -> BtDevice {
        BtDevice {
            address: address.into(),
            name: name.into(),
            transport: BtTransport::LowEnergy,
            input: BtInput::Keyboard,
            rssi: Some(-52),
            bonded: false,
            connected: false,
        }
    }

    #[test]
    fn a_trace_never_carries_a_full_address_or_a_passkey() {
        let d = device("AA:BB:CC:DD:EE:01", "Desk");
        let shown = format!("{d:?}");
        assert!(
            shown.contains("xx:xx:xx:xx:EE:01") && !shown.contains("AA:BB"),
            "{shown}"
        );
        let p = BtPairing {
            phase: BtPhase::ShowPasskey,
            address: "AA:BB:CC:DD:EE:01".into(),
            passkey: 123_456,
            number: 654_321,
            reason: BtReason::None,
        };
        let shown = format!("{:?}", BtEvent::Pairing(p));
        assert!(!shown.contains("123456") && !shown.contains("654321") && !shown.contains("AA:BB"));
        assert_eq!(redacted("garbage"), "xx");
    }

    #[test]
    fn rows_say_what_a_device_is() {
        let mut d = device("AA:BB:CC:DD:EE:01", "Desk keyboard");
        assert_eq!(d.row(), "Desk keyboard   (found, low energy, -52 dBm)");
        d.bonded = true;
        d.connected = true;
        d.rssi = None;
        d.transport = BtTransport::Classic;
        assert_eq!(d.row(), "Desk keyboard   (connected, classic)");
        d.name.clear();
        d.input = BtInput::Pointer;
        assert!(d.row().starts_with("Mouse "));
    }

    #[test]
    fn events_keep_the_list_current_and_bounded() {
        let mut m = BtModel::default();
        m.apply(BtEvent::Device {
            device: device("A", "one"),
            removed: false,
        });
        m.apply(BtEvent::Device {
            device: device("A", "renamed"),
            removed: false,
        });
        assert_eq!(m.devices.len(), 1);
        assert_eq!(m.devices[0].name, "renamed");
        m.apply(BtEvent::Device {
            device: device("A", ""),
            removed: true,
        });
        assert!(m.devices.is_empty());
        for n in 0..(MAX_DEVICES + 10) {
            m.apply(BtEvent::Device {
                device: device(&format!("D{n}"), ""),
                removed: false,
            });
        }
        assert_eq!(m.devices.len(), MAX_DEVICES);
        let many = (0..500).map(|n| device(&format!("S{n}"), "")).collect();
        m.apply(BtEvent::Snapshot {
            controller: true,
            discovering: false,
            devices: many,
            pairing: BtPairing::idle(),
        });
        assert_eq!(m.devices.len(), MAX_DEVICES);
        assert!(m.controller && m.problem.is_none());
    }

    #[test]
    fn a_pairing_is_active_until_it_ends() {
        let mut p = BtPairing::idle();
        assert!(!p.active());
        for phase in [
            BtPhase::Connecting,
            BtPhase::ShowPasskey,
            BtPhase::ConfirmNumber,
        ] {
            p.phase = phase;
            assert!(p.active());
        }
        for phase in [BtPhase::Paired, BtPhase::Failed, BtPhase::Idle] {
            p.phase = phase;
            assert!(!p.active());
        }
    }
}
