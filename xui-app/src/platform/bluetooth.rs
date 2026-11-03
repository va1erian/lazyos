//! The Settings app's [`Bluetooth`] on LazyOS: `btd` over `os.lazy.bt.v1`
//! (`idl/bt.midl`, docs/bluetooth-plan.md B5).
//!
//! **The window never waits on `btd`.** The trait's calls queue a request for
//! the command thread and return; that thread makes the (bounded) Messenger
//! calls and answers with [`BtEvent`]s through the window's sink. A second
//! thread blocks on the event channel `btd` was handed in `Subscribe` and
//! forwards what it sends (a device found, the passkey to show) the same way;
//! nothing polls on a timer. Both threads are worker threads
//! ([`mark_worker`]), and the passkey travels only to the window: it is never
//! printed.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;

use libmessenger::{Parcel, VERSION};
use messenger_generated::errors::ERROR_FIELD;
use messenger_generated::os_lazy_bt_v1 as wire;
use xui_settings::bluetooth::{
    Bluetooth, BtDevice, BtEvent, BtInput, BtPairing, BtPhase, BtReason, BtSink, BtTransport,
};

use super::messenger::{mark_worker, CallError, Service};
use crate::sys::{self, errno};

/// `btd`'s registered service name.
const NAME: &str = "os.lazy.bt";
/// PIT ticks a call may take (100 Hz): `btd` answers from memory.
const CALL_TICKS: u64 = 500;
/// How long a scan runs when asked (seconds); the page stops it earlier.
const SCAN_SECONDS: u32 = 30;
/// The largest event parcel read.
const EVENT_BUF: usize = 16 * 1024;

/// What the window asked.
enum Cmd {
    Refresh,
    Scan,
    StopScan,
    Pair(String),
    Confirm(String, bool),
    Cancel,
    Forget(String),
}

/// The live service.
#[derive(Default)]
pub struct OsBluetooth {
    commands: RefCell<Option<Sender<Cmd>>>,
}

impl OsBluetooth {
    pub fn new() -> OsBluetooth {
        OsBluetooth::default()
    }

    fn send(&self, cmd: Cmd) {
        if let Some(tx) = self.commands.borrow().as_ref() {
            let _ = tx.send(cmd);
        }
    }
}

impl Bluetooth for OsBluetooth {
    fn start(&self, sink: BtSink) {
        if self.commands.borrow().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("settings-bt".into())
            .spawn(move || run(rx, sink));
        if spawned.is_ok() {
            self.commands.replace(Some(tx));
            self.send(Cmd::Refresh);
        }
    }

    fn refresh(&self) {
        self.send(Cmd::Refresh);
    }
    fn scan(&self) {
        self.send(Cmd::Scan);
    }
    fn stop_scan(&self) {
        self.send(Cmd::StopScan);
    }
    fn pair(&self, address: &str) {
        self.send(Cmd::Pair(address.into()));
    }
    fn confirm(&self, address: &str, accept: bool) {
        self.send(Cmd::Confirm(address.into(), accept));
    }
    fn cancel(&self) {
        self.send(Cmd::Cancel);
    }
    fn forget(&self, address: &str) {
        self.send(Cmd::Forget(address.into()));
    }
}

/// The command thread: connect, subscribe, answer requests until the window
/// (the sender) is gone.
fn run(rx: Receiver<Cmd>, sink: BtSink) {
    mark_worker();
    let mut link = Link::default();
    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, Cmd::Refresh) {
            link.connect(&sink);
            continue;
        }
        let Some(service) = link.service.as_ref() else {
            sink(BtEvent::Failed("Bluetooth is not available.".into()));
            continue;
        };
        let refusal = match cmd {
            Cmd::Scan => call(service, wire::METHOD_STARTDISCOVERY, scan_body()).map(|_| {
                sink(BtEvent::Discovering(true));
            }),
            Cmd::StopScan => call(service, wire::METHOD_STOPDISCOVERY, Ok(Vec::new())).map(|_| {
                sink(BtEvent::Discovering(false));
            }),
            Cmd::Pair(address) => call(service, wire::METHOD_PAIR, pair_body(&address)).map(drop),
            Cmd::Confirm(address, accept) => call(
                service,
                wire::METHOD_CONFIRMNUMBER,
                confirm_body(&address, accept),
            )
            .map(drop),
            Cmd::Cancel => call(service, wire::METHOD_CANCELPAIRING, Ok(Vec::new())).map(drop),
            Cmd::Forget(address) => {
                call(service, wire::METHOD_FORGET, forget_body(&address)).map(drop)
            }
            Cmd::Refresh => Ok(()),
        };
        if let Err(error) = refusal {
            sink(BtEvent::Failed(explain(&error)));
            if error.code == -errno::EPIPE || error.code == -errno::ENOENT {
                link.service = None;
            }
        }
    }
}

/// The connection to `btd` and its event thread.
#[derive(Default)]
struct Link {
    service: Option<Service>,
    subscribed: bool,
}

impl Link {
    /// (Re)connect, subscribe once and send a snapshot.
    fn connect(&mut self, sink: &BtSink) {
        if self.service.is_none() {
            self.subscribed = false;
            match Service::try_connect(NAME) {
                Some(service) => self.service = Some(service),
                None => {
                    return sink(BtEvent::Snapshot {
                        controller: false,
                        discovering: false,
                        devices: Vec::new(),
                        pairing: BtPairing::idle(),
                    })
                }
            }
        }
        let Some(service) = self.service.as_ref() else {
            return;
        };
        if !self.subscribed {
            match subscribe(service, sink) {
                Ok(()) => self.subscribed = true,
                Err(error) => sink(BtEvent::Failed(explain(&error))),
            }
        }
        match snapshot(service) {
            Ok(event) => sink(event),
            Err(error) => {
                sink(BtEvent::Failed(explain(&error)));
                self.service = None;
            }
        }
    }
}

fn call(
    service: &Service,
    method: u32,
    body: Result<Vec<u8>, CallError>,
) -> Result<Parcel, CallError> {
    service.call_detailed(wire::INTERFACE_ID, method, ERROR_FIELD, body?, CALL_TICKS)
}

fn encode_error<E: core::fmt::Debug>(_: E) -> CallError {
    CallError {
        code: -errno::EINVAL,
        message: String::from("the request could not be encoded"),
    }
}

fn scan_body() -> Result<Vec<u8>, CallError> {
    wire::encode_start_discovery_args(&wire::StartDiscoveryArgs {
        seconds: SCAN_SECONDS,
    })
    .map_err(encode_error)
}

fn pair_body(address: &str) -> Result<Vec<u8>, CallError> {
    wire::encode_pair_args(&wire::PairArgs {
        address: address.into(),
    })
    .map_err(encode_error)
}

fn confirm_body(address: &str, accept: bool) -> Result<Vec<u8>, CallError> {
    wire::encode_confirm_number_args(&wire::ConfirmNumberArgs {
        address: address.into(),
        accept,
    })
    .map_err(encode_error)
}

fn forget_body(address: &str) -> Result<Vec<u8>, CallError> {
    wire::encode_forget_args(&wire::ForgetArgs {
        address: address.into(),
    })
    .map_err(encode_error)
}

/// The text a refusal shows.
fn explain(error: &CallError) -> String {
    match -error.code {
        errno::EACCES => "Only the person signed in at this computer may use Bluetooth.".into(),
        errno::ENODEV => "No Bluetooth controller was found.".into(),
        errno::EBUSY => "Another pairing is in progress.".into(),
        errno::ENOENT => "That device is not known (scan again).".into(),
        errno::EPIPE => "The Bluetooth service stopped.".into(),
        _ if !error.message.is_empty() => format!("Bluetooth: {}.", error.message),
        _ => "The Bluetooth request failed.".into(),
    }
}

/// `List` and `Status` as one snapshot.
fn snapshot(service: &Service) -> Result<BtEvent, CallError> {
    let list = call(service, wire::METHOD_LIST, Ok(Vec::new()))?;
    let list = wire::decode_list_reply(&list.body).map_err(encode_error)?;
    let status = call(service, wire::METHOD_STATUS, Ok(Vec::new()))?;
    let status = wire::decode_status_reply(&status.body).map_err(encode_error)?;
    Ok(BtEvent::Snapshot {
        controller: list.controller,
        discovering: list.discovering,
        devices: list.devices.iter().map(device).collect(),
        pairing: pairing(&status.pairing),
    })
}

/// `Subscribe`: hand `btd` one end of a fresh channel and start the thread
/// that reads the other.
fn subscribe(service: &Service, sink: &BtSink) -> Result<(), CallError> {
    let (mine, theirs) = sys::msg_create_pair().map_err(|code| CallError {
        code,
        message: String::new(),
    })?;
    let result = wire::encode_subscribe_args(&wire::SubscribeArgs { events: theirs })
        .map_err(encode_error)
        .and_then(|(body, objects)| {
            service
                .call_moving_within(
                    wire::INTERFACE_ID,
                    wire::METHOD_SUBSCRIBE,
                    ERROR_FIELD,
                    body,
                    objects,
                    CALL_TICKS,
                )
                .map_err(|code| CallError {
                    code,
                    message: String::new(),
                })
        });
    if let Err(error) = result {
        let _ = sys::msg_close(mine);
        let _ = sys::msg_close(theirs);
        return Err(error);
    }
    let sink = Arc::clone(sink);
    let spawned = thread::Builder::new()
        .name("settings-bt-events".into())
        .spawn(move || listen(mine, sink));
    spawned.map(drop).map_err(|_| CallError {
        code: -errno::ENOMEM,
        message: String::new(),
    })
}

/// Block on the event channel until `btd` goes away.
fn listen(events: u64, sink: BtSink) {
    mark_worker();
    let mut buf = vec![0u8; EVENT_BUF];
    // `0`: wait for ever; a closed peer ends the wait with an error.
    while let Ok(result) = sys::msg_recv(events, &mut buf, 0) {
        let Some(bytes) = buf.get(..result.bytes as usize) else {
            continue;
        };
        let Ok(parcel) = Parcel::decode(bytes) else {
            continue;
        };
        if parcel.header.version != VERSION || parcel.header.interface_id != wire::INTERFACE_ID {
            continue;
        }
        match parcel.header.method {
            wire::METHOD_DEVICEEVENT => {
                if let Ok(args) = wire::decode_device_event_args(&parcel.body) {
                    sink(BtEvent::Device {
                        device: device(&args.device),
                        removed: args.kind == wire::DEVICE_EVENT_KIND_REMOVED,
                    });
                }
            }
            wire::METHOD_PAIRINGEVENT => {
                if let Ok(args) = wire::decode_pairing_event_args(&parcel.body) {
                    sink(BtEvent::Pairing(pairing(&args.pairing)));
                }
            }
            _ => {}
        }
    }
    let _ = sys::msg_close(events);
    sink(BtEvent::Failed("The Bluetooth service stopped.".into()));
}

/// A device from the wire. Names are text from the air: the service cleaned
/// them, and a name is shown as given, never interpreted.
fn device(d: &wire::Device) -> BtDevice {
    BtDevice {
        address: d.address.clone(),
        name: d.name.clone(),
        transport: if d.transport == wire::TRANSPORT_LE {
            BtTransport::LowEnergy
        } else {
            BtTransport::Classic
        },
        input: match d.input {
            wire::INPUT_CLASS_KEYBOARD => BtInput::Keyboard,
            wire::INPUT_CLASS_POINTER => BtInput::Pointer,
            _ => BtInput::Unknown,
        },
        rssi: d.rssi,
        bonded: d.bonded,
        connected: d.connected,
    }
}

fn pairing(p: &wire::Pairing) -> BtPairing {
    BtPairing {
        phase: match p.phase {
            wire::PHASE_CONNECTING => BtPhase::Connecting,
            wire::PHASE_SHOW_PASSKEY => BtPhase::ShowPasskey,
            wire::PHASE_CONFIRM_NUMBER => BtPhase::ConfirmNumber,
            wire::PHASE_PAIRED => BtPhase::Paired,
            wire::PHASE_FAILED => BtPhase::Failed,
            _ => BtPhase::Idle,
        },
        address: p.address.clone(),
        passkey: p.passkey,
        number: p.number,
        reason: match p.reason {
            wire::FAIL_REASON_TIMEOUT => BtReason::Timeout,
            wire::FAIL_REASON_CANCELLED => BtReason::Cancelled,
            wire::FAIL_REASON_REJECTED => BtReason::Rejected,
            wire::FAIL_REASON_AUTH_FAILED => BtReason::AuthFailed,
            wire::FAIL_REASON_UNSUPPORTED => BtReason::Unsupported,
            wire::FAIL_REASON_LINK_LOST => BtReason::LinkLost,
            _ => BtReason::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_keeps_what_the_list_shows_and_nothing_else() {
        let wire_device = wire::Device {
            address: "C0:FF:EE:00:01:01".into(),
            name: "Sim Keyboard".into(),
            transport: wire::TRANSPORT_LE,
            input: wire::INPUT_CLASS_KEYBOARD,
            rssi: Some(-50),
            bonded: true,
            connected: false,
        };
        let d = device(&wire_device);
        assert_eq!(d.transport, BtTransport::LowEnergy);
        assert_eq!(d.input, BtInput::Keyboard);
        assert_eq!((d.rssi, d.bonded, d.connected), (Some(-50), true, false));
    }

    #[test]
    fn unknown_wire_values_are_harmless() {
        let p = pairing(&wire::Pairing {
            phase: 99,
            address: String::new(),
            passkey: 0,
            number: 0,
            reason: 99,
        });
        assert_eq!((p.phase, p.reason), (BtPhase::Idle, BtReason::None));
    }

    #[test]
    fn refusals_read_as_sentences() {
        let e = |code| CallError {
            code: -code,
            message: String::new(),
        };
        assert!(explain(&e(errno::EACCES)).contains("signed in"));
        assert!(explain(&e(errno::ENODEV)).contains("controller"));
        assert!(explain(&e(errno::EBUSY)).contains("in progress"));
        assert_eq!(explain(&e(errno::EIO)), "The Bluetooth request failed.");
    }
}
