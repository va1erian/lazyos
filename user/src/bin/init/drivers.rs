//! Driver rows started on request (issue #497, docs/driver-plan.md 3.6).
//!
//! With `devd` in the image, `init` no longer starts `netdrv` and `sndd` at
//! boot: they are rows it *knows* (program, credentials, arguments, restart
//! policy) and starts when `devd` names one together with the device it
//! matched. Only the running task of the `devd` row may ask, and it can name
//! nothing but a row and a device id, so a compromised `devd` can at worst
//! start a known driver for a device that driver then fails to claim.
//! The device reaches the driver as one more argument, `dev=<id>`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use user::trace;

use user::messenger::{self, errno, router, services, Message, Parcel};

use super::service::{Phase, Service};
use super::shutdown;
use super::state::ServiceSpec;
use super::supervise::spawn_service;

/// The driver rows `devd` may ask for.
pub(super) const DRIVER_ROWS: &[ServiceSpec] = &[
    #[cfg(lazyos_sound)]
    super::state::SNDD_ROW,
    #[cfg(lazyos_net)]
    super::state::NETDRV_ROW,
    #[cfg(lazyos_wifi)]
    WIFID_ROW,
    #[cfg(lazyos_gen9gfx)]
    GPUD_ROW,
];

/// The Intel GPU service (docs/kabylake-gpu-plan.md, stage G1), present only
/// on `LAZYOS_GEN9GFX=1` images and started only by `devd`, for the HD
/// Graphics 620 it found; it runs as `_gpu` with `CAP_DEV_CLAIM`
/// (`service_creds::GPU_CRED`). Started by hand without the device it says
/// `GPU:NO-DEVICE:FAIL` and exits 0, so `OnFailure` restarts it only after a
/// crash, and it never exits on purpose while it holds the claim (a release
/// switches the function's memory decode off under the kernel console).
/// `dbgctl reload gpud` replaces its binary (`reload.rs`).
#[cfg(lazyos_gen9gfx)]
pub(super) const GPUD_ROW: ServiceSpec = ServiceSpec {
    name: "gpud",
    path: fhs::bin::GPUD,
    args: "",
    restart: super::state::Restart::OnFailure,
    deps: &[],
};

/// The Wi-Fi driver (docs/wifi-rtw88-plan.md), present only on
/// `LAZYOS_WIFI=1` images and started only by `devd`, for an RTL8822CE it
/// found, as `ifname=wlanN`; it runs as `_wifi` with `CAP_DEV_CLAIM`
/// (`service_creds::WIFI_CRED`). Started by hand without a card it says
/// `WIFID:FAIL:no-device` and exits 0, so `OnFailure` restarts it only after
/// a crash. `dbgctl reload wifid` replaces its binary (`reload.rs`).
#[cfg(lazyos_wifi)]
pub(super) const WIFID_ROW: ServiceSpec = ServiceSpec {
    name: "wifid",
    path: fhs::bin::WIFID,
    args: "",
    restart: super::state::Restart::OnFailure,
    deps: &[],
};

/// Whether a row's task is or will be running (it holds the driver's place).
fn active(row: &Service) -> bool {
    matches!(
        row.phase,
        Phase::Pending | Phase::Running | Phase::Restarting
    )
}

fn errno_error(code: i64) -> messenger::Error {
    messenger::Error::Errno(-code)
}

/// `StartDriver(driver, device)`.
pub(super) fn start(
    services: &mut Vec<Service>,
    broker: &mut router::TopicBroker,
    message: &Message,
) -> messenger::Result<Parcel> {
    let request = services::init::wire::decode_start_driver_args(&message.parcel.body)
        .map_err(messenger::Error::Parcel)?;
    let from_devd = services.iter().any(|row| {
        !row.launched
            && row.name == "devd"
            && row.phase == Phase::Running
            && row.pid == message.sender
    });
    if !from_devd {
        trace::line!(
            trace::Channel::Init,
            "DRIVER:DENIED driver={} caller={}",
            request.driver,
            message.sender
        );
        return Err(errno_error(errno::EPERM));
    }
    if shutdown::stopping() {
        return Err(errno_error(errno::EBUSY));
    }
    let spec = DRIVER_ROWS
        .iter()
        .find(|spec| spec.name == request.driver)
        .ok_or_else(|| errno_error(errno::ENOENT))?;
    let device_arg = format!("dev={}", request.device);
    // A driver that names its card runs once per card; any other has a unique
    // Messenger name and so serves one device.
    let named = !request.ifname.is_empty();
    if named && !devmatch::valid_ifname(&request.ifname) {
        return Err(errno_error(errno::EINVAL));
    }
    let ifname_arg = format!("ifname={}", request.ifname);
    let mut driver_rows = services
        .iter()
        .filter(|row| !row.launched && row.name == spec.name && active(row));
    if named {
        if let Some(row) = driver_rows
            .clone()
            .find(|row| row.args.contains(&device_arg))
        {
            return reply(false, row.pid);
        }
        // Two cards under one name would register one Messenger name twice.
        if driver_rows.any(|row| row.args.contains(&ifname_arg)) {
            return Err(errno_error(errno::EINVAL));
        }
    } else if let Some(row) = driver_rows.next() {
        if !row.args.contains(&device_arg) {
            return Err(errno_error(errno::EBUSY));
        }
        return reply(false, row.pid);
    }
    let mut row = Service::from_manifest(spec);
    row.args.push(String::from(device_arg.as_str()));
    if named {
        row.args.push(ifname_arg);
    }
    services.push(row);
    let index = services.len() - 1;
    spawn_service(services, index, broker);
    trace::line!(
        trace::Channel::Init,
        "DRIVER:START driver={} {device_arg} pid={} ifname={}",
        spec.name,
        services[index].pid,
        request.ifname
    );
    reply(true, services[index].pid)
}

fn reply(started: bool, pid: u64) -> messenger::Result<Parcel> {
    services::drivers::start_driver_reply(started, pid)
}
