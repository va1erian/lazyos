//! `btd`'s service side: `os.lazy.bt.v1` (`idl/bt.midl`, docs/bluetooth-plan.md
//! B5). The name is registered for as long as `btd` runs, controller or not,
//! so a client always gets an answer (`controller: false`, `ENODEV`) and
//! never a missing name.
//!
//! Every request is checked against the kernel-stamped caller
//! (`btpolicy::may_use_api`) before the host sees it, and the host's answer
//! is the only thing sent back: no key leaves this process. The host emits
//! [`Effect::Device`] and [`Effect::Pairing`] notices; [`Api::broadcast`]
//! sends them to the subscribers, who each handed over an event channel. A
//! subscriber whose channel closed is dropped; one whose channel is full
//! misses the event (the `List` and `Status` calls are always the truth).
//!
//! Serial: `BT:API:SERVE`, `BT:API:DENY method=<n> uid=<uid>` (a refusal, rate
//! limited), `BT:API:<call> addr=xx:xx:xx:xx:AA:BB` for the calls that change
//! state. Addresses are redacted as everywhere in `btd`'s traces.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use btpolicy::{may_use_api, Caller, Op};
use bthost::ctl_types::{
    CtlError, DeviceChange, DeviceInfo, FailReason, InputKind, PairingState, Phase, Transport,
};
use bthost::Host;
use user::messenger::bt::{self as api, wire};
use user::messenger::{self, errno, registry, Endpoint, Error as MsgError, Message, Parcel};
use user::trace::{self, Channel};

/// Requests served per pass at most, so a flood cannot starve the radio.
const BATCH: usize = 16;

/// Refusals traced per window: a caller that is not allowed can knock as often
/// as it likes and the console is not its to fill.
static DENIALS: tracefmt::Budget = tracefmt::Budget::new(8, 6000);

type Result<T> = core::result::Result<T, MsgError>;

fn err(code: i64) -> MsgError {
    MsgError::Errno(-code)
}

pub(super) struct Api {
    /// The published and the serving end, while the name is registered.
    ends: Option<(Endpoint, Endpoint)>,
    subscribers: Vec<Endpoint>,
    buffer: Vec<u8>,
}

impl Api {
    pub(super) fn new() -> Api {
        Api {
            ends: None,
            subscribers: Vec::new(),
            buffer: vec![0u8; messenger::DEFAULT_BUFFER],
        }
    }

    /// Register the name (once). A refusal is traced and retried by the next
    /// call, so a registry that is not up yet does not end the service.
    pub(super) fn register(&mut self) {
        if self.ends.is_some() {
            return;
        }
        let Ok((published, server)) = messenger::create_pair() else {
            return;
        };
        match registry::register(api::NAME, &published, &[api::INTERFACE], 0) {
            Ok(()) => {
                trace::line!(Channel::Bt, "API:SERVE name={}", api::NAME);
                self.ends = Some((published, server));
            }
            Err(error) => {
                trace::line!(Channel::Bt, "API:FAIL register {}", error.message());
                let _ = published.close();
                let _ = server.close();
            }
        }
    }

    /// The serving end, for the wait set.
    pub(super) fn endpoint(&self) -> Option<Endpoint> {
        self.ends.map(|(_, server)| server)
    }

    /// Wait up to `ticks` with no controller attached, answering requests as
    /// they come (a call must not wait for a controller that is not there).
    pub(super) fn idle(&mut self, ticks: u64) {
        let deadline = user::sys::clock() + ticks;
        loop {
            let now = user::sys::clock();
            if now >= deadline {
                return;
            }
            match self.endpoint() {
                Some(server) => {
                    let _ = messenger::wait::wait_any(&[server], 0, Some(deadline));
                    self.serve(None, 0);
                }
                None => {
                    user::sys::wait(deadline);
                    self.register();
                }
            }
        }
    }

    /// Answer the queued requests against `host` (`None` while no controller
    /// is attached); whether there were any.
    pub(super) fn serve(&mut self, host: Option<&mut Host>, now: u64) -> bool {
        let Some((_, server)) = self.ends else {
            return false;
        };
        let mut host = host;
        let mut served = false;
        for _ in 0..BATCH {
            let message = match server.poll_recv_with(&mut self.buffer) {
                Ok(Some(message)) => message,
                _ => break,
            };
            served = true;
            let reply = self.dispatch(host.as_deref_mut(), now, &message);
            let Some(txn) = message.txn else {
                continue;
            };
            let reply = reply.unwrap_or_else(|error| {
                let code = error.errno().map_or(errno::EINVAL, |code| -code);
                api::error_reply(message.method(), code, reason(code))
            });
            let _ = server.reply_or_drop(txn, &reply);
        }
        served
    }

    fn dispatch(&mut self, host: Option<&mut Host>, now: u64, message: &Message) -> Result<Parcel> {
        if message.interface_id() != api::INTERFACE {
            return Err(err(errno::EINVAL));
        }
        let method = message.method();
        let cred = message.caller();
        let caller = Caller {
            uid: cred.uid,
            label_id: cred.label_id,
            session: cred.session,
        };
        let Some(op) = op_of(method) else {
            return Err(err(errno::EINVAL));
        };
        if !may_use_api(caller, op) {
            if trace::within(&DENIALS, Channel::Bt) {
                trace::line!(Channel::Bt, "API:DENY method={method} uid={}", caller.uid);
            }
            return Err(err(errno::EACCES));
        }
        let body = &message.parcel.body;
        let reply = match method {
            wire::METHOD_LIST => encode_list(host.as_deref()),
            wire::METHOD_STATUS => wire::encode_status_reply(&wire::StatusReply {
                pairing: pairing_to_wire(
                    host.as_deref()
                        .map_or(PairingState::IDLE, Host::pairing_state),
                ),
            })
            .map_err(MsgError::Parcel),
            wire::METHOD_SUBSCRIBE => {
                let args = message.decode(wire::decode_subscribe_args)?;
                self.subscribe(Endpoint::from_raw(args.events))?;
                Ok(Vec::new())
            }
            _ => {
                let host = host.ok_or(err(errno::ENODEV))?;
                control(host, now, method, body)
            }
        }?;
        Ok(api::parcel(method, reply, Vec::new()))
    }

    fn subscribe(&mut self, events: Endpoint) -> Result<()> {
        if self.subscribers.len() >= api::MAX_SUBSCRIBERS {
            let _ = events.close();
            return Err(err(errno::EBUSY));
        }
        self.subscribers.push(events);
        Ok(())
    }

    /// Send a notice from the host to every subscriber.
    pub(super) fn broadcast(&mut self, device: Option<(DeviceChange, &DeviceInfo)>, state: Option<&PairingState>) {
        let parcel = match (device, state) {
            (Some((change, info)), _) => wire::encode_device_event_args(&wire::DeviceEventArgs {
                kind: change_to_wire(change),
                device: device_to_wire(info),
            })
            .map(|body| api::event(wire::METHOD_DEVICEEVENT, body)),
            (_, Some(state)) => wire::encode_pairing_event_args(&wire::PairingEventArgs {
                pairing: pairing_to_wire(*state),
            })
            .map(|body| api::event(wire::METHOD_PAIRINGEVENT, body)),
            _ => return,
        };
        let Ok(parcel) = parcel else {
            return;
        };
        self.subscribers.retain(|events| match events.send(&parcel) {
            Ok(()) => true,
            // A full channel misses the event; anything else is a closed one.
            Err(error) => error.errno() == Some(-errno::EAGAIN),
        });
    }
}

fn op_of(method: u32) -> Option<Op> {
    Some(match method {
        wire::METHOD_LIST => Op::List,
        wire::METHOD_STATUS | wire::METHOD_SUBSCRIBE => Op::Observe,
        wire::METHOD_STARTDISCOVERY
        | wire::METHOD_STOPDISCOVERY
        | wire::METHOD_PAIR
        | wire::METHOD_CONFIRMNUMBER
        | wire::METHOD_CANCELPAIRING
        | wire::METHOD_FORGET => Op::Control,
        _ => return None,
    })
}

/// The calls that act on the host.
fn control(host: &mut Host, now: u64, method: u32, body: &[u8]) -> Result<Vec<u8>> {
    let parse = |address: &str| bthost::config::parse_addr(address).ok_or(err(errno::EINVAL));
    let done = match method {
        wire::METHOD_STARTDISCOVERY => {
            let args = wire::decode_start_discovery_args(body).map_err(MsgError::Parcel)?;
            host.start_discovery(now, args.seconds)
        }
        wire::METHOD_STOPDISCOVERY => {
            host.stop_discovery(now);
            Ok(())
        }
        wire::METHOD_PAIR => {
            let addr = parse(&wire::decode_pair_args(body).map_err(MsgError::Parcel)?.address)?;
            trace::line!(Channel::Bt, "API:PAIR addr={}", addr.redacted());
            host.pair(now, addr)
        }
        wire::METHOD_CONFIRMNUMBER => {
            let args = wire::decode_confirm_number_args(body).map_err(MsgError::Parcel)?;
            host.confirm_number(now, parse(&args.address)?, args.accept)
        }
        wire::METHOD_CANCELPAIRING => {
            trace::line!(Channel::Bt, "API:CANCEL");
            host.cancel_pairing(now);
            Ok(())
        }
        wire::METHOD_FORGET => {
            let addr = parse(&wire::decode_forget_args(body).map_err(MsgError::Parcel)?.address)?;
            trace::line!(Channel::Bt, "API:FORGET addr={}", addr.redacted());
            host.forget_device(now, addr)
        }
        _ => return Err(err(errno::EINVAL)),
    };
    done.map(|()| Vec::new()).map_err(|e| err(errno_of(e)))
}

fn errno_of(error: CtlError) -> i64 {
    match error {
        CtlError::NoController => errno::ENODEV,
        CtlError::Busy => errno::EBUSY,
        CtlError::NotFound => errno::ENOENT,
        CtlError::Invalid => errno::EINVAL,
    }
}

/// A short, non-secret reason for a refusal.
fn reason(code: i64) -> &'static str {
    match code {
        errno::EACCES => "not allowed",
        errno::ENODEV => "no Bluetooth controller",
        errno::EBUSY => "busy",
        errno::ENOENT => "no such device",
        _ => "invalid request",
    }
}

fn encode_list(host: Option<&Host>) -> Result<Vec<u8>> {
    let devices = host.map_or_else(Vec::new, |h| {
        h.devices().iter().map(device_to_wire).collect()
    });
    wire::encode_list_reply(&wire::ListReply {
        devices,
        controller: host.is_some_and(Host::is_armed),
        discovering: host.is_some_and(Host::is_discovering),
    })
    .map_err(MsgError::Parcel)
}

fn change_to_wire(change: DeviceChange) -> u32 {
    match change {
        DeviceChange::Found => wire::DEVICE_EVENT_KIND_FOUND,
        DeviceChange::Changed => wire::DEVICE_EVENT_KIND_CHANGED,
        DeviceChange::Removed => wire::DEVICE_EVENT_KIND_REMOVED,
    }
}

fn device_to_wire(info: &DeviceInfo) -> api::Device {
    api::Device {
        address: info.addr.to_string(),
        name: info.name.clone(),
        transport: match info.transport {
            Transport::BrEdr => wire::TRANSPORT_BR_EDR,
            Transport::Le => wire::TRANSPORT_LE,
        },
        input: match info.input {
            InputKind::Unknown => wire::INPUT_CLASS_UNKNOWN,
            InputKind::Keyboard => wire::INPUT_CLASS_KEYBOARD,
            InputKind::Pointer => wire::INPUT_CLASS_POINTER,
        },
        rssi: info.rssi.map(i32::from),
        bonded: info.bonded,
        connected: info.connected,
    }
}

fn pairing_to_wire(state: PairingState) -> api::Pairing {
    api::Pairing {
        phase: match state.phase {
            Phase::Idle => wire::PHASE_IDLE,
            Phase::Connecting => wire::PHASE_CONNECTING,
            Phase::ShowPasskey => wire::PHASE_SHOW_PASSKEY,
            Phase::ConfirmNumber => wire::PHASE_CONFIRM_NUMBER,
            Phase::Paired => wire::PHASE_PAIRED,
            Phase::Failed => wire::PHASE_FAILED,
        },
        address: state.addr.map(|a| a.to_string()).unwrap_or_else(String::new),
        passkey: state.passkey,
        number: state.number,
        reason: match state.reason {
            FailReason::None => wire::FAIL_REASON_NONE,
            FailReason::Timeout => wire::FAIL_REASON_TIMEOUT,
            FailReason::Cancelled => wire::FAIL_REASON_CANCELLED,
            FailReason::Rejected => wire::FAIL_REASON_REJECTED,
            FailReason::AuthFailed => wire::FAIL_REASON_AUTH_FAILED,
            FailReason::Unsupported => wire::FAIL_REASON_UNSUPPORTED,
            FailReason::LinkLost => wire::FAIL_REASON_LINK_LOST,
        },
    }
}
