//! `devd` (`/system/bin/devd`): the device manager (issue #497,
//! `docs/driver-plan.md` section 3.6).
//!
//! At start it reads the kernel's read-only device inventory, matches every
//! function against the static driver manifest (`libs/devmatch`), and asks
//! `init` to start each matched driver row for the device it found
//! (`os.lazy.init.v1.StartDriver`; the driver gets `dev=<id>`, and a network
//! card also `ifname=<name>`: `eth0`, `eth1`, ... for wired cards and `wlan0`,
//! `wlan1`, ... for wireless ones, each in enumeration order). It then serves
//! `os.lazy.devd.v1` (`idl/devd.midl`) and keeps one retained topic per device,
//! `system/devices/<id>`, current: each device's match, its driver, who holds
//! the claim and whether its driver let it go. A driver that crashes is
//! restarted by `init` (its row's policy), not by `devd`.
//!
//! `devd` runs as `_devd` with **no** capabilities: it never claims, maps or
//! touches a device, and `init` holds every driver's program, credentials and
//! arguments, so all `devd` can do is ask for a known row for a device.
//!
//! Boot evidence: one `DEVD:MATCH` and one `DEVD:START` per driver it starts,
//! `DEVD:READY`, a `DEVD:DEVICE` line per state change, and `DEVD:CLAIMED:PASS`
//! once every started driver holds its device.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::trace;

use user::central;
use user::messenger::devd::{self as api, wire};
use user::messenger::services::{self, drivers, INIT_NAME};
use user::messenger::{self, errno, registry, Endpoint, Error as MsgError, Message, Parcel};
use user::sys;

#[path = "devd/devices.rs"]
mod devices;

use devices::{State, Tracked};

/// Ticks between inventory reads (100 Hz): devices change only when a driver
/// claims or lets go, so twice a second is plenty.
const RESCAN_TICKS: u64 = 50;
/// Attempts to reach `init` and the broker at start (a tick apart).
const CONNECT_ATTEMPTS: usize = 200;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match sys::cred_get(None) {
        Ok(cred) => trace::line!(
            trace::Channel::Devd,
            "CRED uid={} caps={:#x}",
            cred.uid,
            cred.caps
        ),
        Err(code) => trace::line!(trace::Channel::Devd, "CRED unavailable (errno {code})"),
    }
    match run() {
        Ok(()) => sys::exit(0),
        Err(why) => {
            trace::line!(trace::Channel::Devd, "FAIL {why}");
            sys::exit(1)
        }
    }
}

/// Resolve `init`, retrying while the boot is still registering names.
fn init_endpoint() -> Result<Endpoint, &'static str> {
    for _ in 0..CONNECT_ATTEMPTS {
        if let Ok(endpoint) = services::resolve_service(INIT_NAME) {
            return Ok(endpoint);
        }
        sys::nap();
    }
    Err("init is not reachable")
}

fn run() -> Result<(), &'static str> {
    let rows = user::dev::inspect::inventory().map_err(|_| "the device inventory is refused")?;
    let mut tracked = devices::track(rows);
    let init = init_endpoint()?;
    start_drivers(&init, &mut tracked);
    // Released: a resolved name aliases `init`'s own endpoint.
    let _ = init.release();

    let (published, server) = messenger::create_pair().map_err(|_| "no endpoint")?;
    registry::register(api::NAME, &published, &[api::INTERFACE], 0)
        .map_err(|_| "cannot register os.lazy.devd")?;
    let matched = tracked.iter().filter(|t| t.entry.is_some()).count();
    trace::line!(
        trace::Channel::Devd,
        "READY name={} devices={} matched={matched}",
        api::NAME,
        tracked.len()
    );
    services::init::notify_ready();
    let mut bus = central::Bus::connect_retry(CONNECT_ATTEMPTS).ok();
    publish_changes(&mut bus, &mut tracked);
    serve(&server, &mut bus, &mut tracked)
}

/// Ask `init` for each driver start the manifest plans; a device of a driver
/// row that serves one device only, after the first, is `busy`.
fn start_drivers(init: &Endpoint, tracked: &mut [Tracked]) {
    let functions: Vec<_> = tracked.iter().map(Tracked::function).collect();
    let plan = devmatch::plan(&functions);
    for item in tracked.iter_mut().filter(|t| t.entry.is_some()) {
        let id = item.device.id;
        let Some(planned) = plan.iter().find(|planned| planned.id == id) else {
            item.state = State::Busy;
            continue;
        };
        let (driver, ifname) = (planned.driver, planned.ifname.as_str());
        trace::line!(
            trace::Channel::Devd,
            "MATCH id={id} {:04x}:{:04x} class={} driver={driver} model={} ifname={ifname}",
            item.device.vendor,
            item.device.device,
            item.device.class_name(),
            planned.entry.model
        );
        item.ifname = String::from(ifname);
        match drivers::start_driver(init, driver, u64::from(id), ifname) {
            Ok(started) => {
                item.pid = started.pid;
                if item.device.owner.is_none() {
                    item.state = State::Starting;
                }
                trace::line!(
                    trace::Channel::Devd,
                    "START driver={driver} dev={id} ifname={ifname} pid={} new={}",
                    started.pid,
                    started.started
                );
            }
            // The image does not ship this driver (an e1000 QEMU adds by
            // default to a sound-only image): nothing to start, not a fault.
            Err(MsgError::Init(code)) if code == errno::ENOENT => {
                item.state = State::NoDriver;
                trace::line!(
                    trace::Channel::Devd,
                    "NODRIVER driver={driver} dev={id} (not in this image)"
                );
            }
            Err(error) => {
                item.state = State::Failed;
                trace::line!(
                    trace::Channel::Devd,
                    "START:FAIL driver={driver} dev={id} {}",
                    error.message()
                );
            }
        }
    }
}

/// Log every device whose record changed, and publish its retained topic
/// (retried on the next scan when the broker is not there or refuses it).
fn publish_changes(bus: &mut Option<central::Bus>, tracked: &mut [Tracked]) {
    for item in tracked.iter_mut() {
        let record = item.record();
        if item.logged.as_ref() != Some(&record) {
            trace::line!(
                trace::Channel::Devd,
                "DEVICE id={} state={} driver={} owner={}",
                record.id,
                record.state,
                record.driver,
                record.owner
            );
            item.logged = Some(record.clone());
        }
        if item.published.as_ref() == Some(&record) {
            continue;
        }
        let Some(bus) = bus.as_mut() else {
            continue;
        };
        match wire::publish_system_devices(bus, &record.id.to_string(), &record) {
            Ok(_) => item.published = Some(record),
            Err(error) if !item.publish_failed => {
                item.publish_failed = true;
                trace::line!(
                    trace::Channel::Devd,
                    "PUBLISH:ERR id={} {}",
                    record.id,
                    error.message()
                );
            }
            Err(_) => {}
        }
    }
}

/// Evidence that the topics reached the broker: once every started driver
/// holds its device and every record was published, the broker must hold a
/// retained `system/devices/<id>` for each device.
fn check_topics(bus: &mut Option<central::Bus>, tracked: &[Tracked]) -> bool {
    if tracked.iter().any(|item| item.published.is_none()) {
        return false;
    }
    let Some(bus) = bus.as_mut() else {
        return false;
    };
    let Ok(topics) = bus.list() else {
        return false;
    };
    let retained = topics
        .iter()
        .filter(|info| info.retained && info.topic.starts_with("system/devices/"))
        .count();
    if retained >= tracked.len() {
        trace::line!(trace::Channel::Devd, "TOPICS:PASS retained={retained}");
        return true;
    }
    false
}

/// Re-read the inventory and fold it into the tracked states.
fn rescan(tracked: &mut [Tracked], reported: &mut bool) {
    let Ok(rows) = user::dev::inspect::inventory() else {
        return;
    };
    for row in rows {
        if let Some(item) = tracked.iter_mut().find(|t| t.device.id == row.id) {
            item.refresh(row);
        }
    }
    let started: Vec<&Tracked> = tracked
        .iter()
        .filter(|t| matches!(t.state, State::Starting | State::Claimed))
        .collect();
    if !*reported && !started.is_empty() && started.iter().all(|t| t.state == State::Claimed) {
        *reported = true;
        trace::line!(
            trace::Channel::Devd,
            "CLAIMED:PASS drivers={}",
            started.len()
        );
    }
}

fn serve(server: &Endpoint, bus: &mut Option<central::Bus>, tracked: &mut [Tracked]) -> ! {
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    let mut next_scan = sys::clock();
    let mut reported = false;
    let mut topics_checked = false;
    loop {
        match server.recv_with(&mut buffer, Some(next_scan)) {
            Ok(message) => {
                let reply = dispatch(&message, tracked).unwrap_or_else(|error| {
                    services::error_reply(message.interface_id(), message.method(), error)
                });
                if let Some(txn) = message.txn {
                    let _ = server.reply_or_drop(txn, &reply);
                }
            }
            Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(_) => sys::nap(),
        }
        if sys::clock() >= next_scan {
            next_scan = sys::clock() + RESCAN_TICKS;
            rescan(tracked, &mut reported);
            if bus.is_none() {
                *bus = central::Bus::connect().ok();
            }
            publish_changes(bus, tracked);
            if reported && !topics_checked {
                topics_checked = check_topics(bus, tracked);
            }
        }
    }
}

fn dispatch(message: &Message, tracked: &[Tracked]) -> Result<Parcel, MsgError> {
    if message.interface_id() != api::INTERFACE {
        return Err(MsgError::Errno(-errno::EINVAL));
    }
    match message.method() {
        wire::METHOD_DEVICES => {
            let devices = tracked.iter().map(Tracked::record).collect();
            let body = wire::encode_devices_reply(&wire::DevicesReply { devices })
                .map_err(MsgError::Parcel)?;
            Ok(api::parcel(wire::METHOD_DEVICES, body))
        }
        _ => Err(MsgError::Errno(-errno::EINVAL)),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
