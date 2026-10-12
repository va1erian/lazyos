//! `wifid` (`/system/bin/wifid`): the Realtek RTL8822CE Wi-Fi driver
//! (docs/wifi-rtw88-plan.md), as far as first light (stage R3's start).
//!
//! `devd` starts it through `init`'s `wifid` driver row when it finds the
//! card (`10ec:c822`, `libs/devmatch`), with `dev=<id>` and `ifname=wlan0`,
//! as `_wifi` (uid 911) holding only `CAP_DEV_CLAIM`, which the kernel's
//! class rules narrow to wireless cards. It:
//!
//! 1. finds the function and claims it with interrupts on a channel the
//!    kernel makes, switches on memory decode (bus mastering waits, step 5);
//! 2. maps BAR 2 (`bus::Bar`, `rtw88::pci::Mmio`) and reads the chip's
//!    identity from `REG_SYS_CFG1` (`WIFID:CHIP`) and the always-on system
//!    registers (`WIFID:REGS boot`);
//! 3. quiesces the chip a previous driver may have left running
//!    (`rtw88::pci::quiesce`: interrupts masked, DMA stopped, MAC off;
//!    `WIFID:QUIESCE`), then arms MSI (`WIFID:IRQ`);
//! 4. loads `rtw88/rtw8822c_fw.bin` from the firmware tree through
//!    `libs/fwload` (`WIFID:FW:FILE`, with the header's version);
//! 5. switches bus mastering on and hands the bus (registers, DMA through
//!    `dev::dma_alloc`, a delay) to `radio::bring_up`: `libs/rtw88`'s
//!    probe, power-on, firmware download, MAC/BB/RF init and calibration
//!    (`WIFID:CHIP:INFO`, `WIFID:FW:READY`, `WIFID:COEX`, `WIFID:MAC`);
//! 6. serves the interface through the shared soft-MAC (`user::wifi::run`
//!    over `hardware.rs`): `os.lazy.net.wifi.hw/<if>` for `wlanmd` and
//!    `os.lazy.net.nic/<if>` for `netd` (`WIFI:READY`); or, with
//!    `mode=survey`, hops the channels and reports what the radio hears
//!    (`survey.rs`, `WIFID:SURVEY:*`). When the service loop ends the
//!    radio is dropped, which powers the chip off (`WIFID:STOP`), and
//!    `wifid` exits 1 for `init` to restart it.
//!
//! A step that fails before the BAR is mapped (the device list, the claim:
//! `EBUSY` while the kernel still tears down a reloaded driver's claim)
//! prints `WIFID:FAIL:<step>` and exits 1, so `init`'s `OnFailure` row
//! tries again. A step after it prints `WIFID:FAIL:<step> <detail>` and the
//! register dump, then parks with the claim held (a restart could only fail
//! the same way, and `dbgd` can still read the box). Started with no
//! RTL8822CE (by hand, or a stale `dev=`), it prints `WIFID:FAIL:no-device`
//! once and exits 0, which `init`'s `OnFailure` row does not restart. A
//! panic exits without powering the chip off; the next run's quiesce (step
//! 3) covers that. Every line is
//! built with `libs/tracefmt` (`trace.rs`); on the NUC read them with
//! `dbgctl log --source programs --follow`, and replace the binary with
//! `dbgctl reload wifid` (docs/wifi-rtw88-plan.md section 5).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use core::panic::PanicInfo;

use user::dev::Row;
use user::messenger::{wait, Endpoint};
use user::sys;

#[path = "wifid/bus.rs"]
mod bus;
#[path = "wifid/chip.rs"]
mod chip;
#[path = "wifid/device.rs"]
mod device;
#[path = "wifid/firmware.rs"]
mod firmware;
#[path = "wifid/hardware.rs"]
mod hardware;
#[path = "wifid/radio.rs"]
mod radio;
#[path = "wifid/rxmeta.rs"]
mod rxmeta;
#[path = "wifid/survey.rs"]
mod survey;
#[path = "wifid/trace.rs"]
mod trace;

use radio::{Failure, Radio};

/// Longest park between interrupt checks (100 Hz ticks): nothing needs a
/// timer at first light, the wait only bounds how stale the loop can be.
const IDLE_TICKS: u64 = 6000;

/// `LAZYOS_WIFID_MODE` at build time: the mode when the arguments name
/// none (`devd` passes only `dev=` and `ifname=`), so a diagnostic stick for
/// the NUC is a build switch, `LAZYOS_WIFID_MODE=survey`.
const BUILD_MODE: Option<&str> = option_env!("LAZYOS_WIFID_MODE");

struct Args {
    /// `dev=<id>`: the device `devd` matched this driver to.
    dev: Option<u64>,
    /// `ifname=<name>`: the interface name `devd` gave the card.
    ifname: String,
    /// `mode=survey`: hop the channels and report what the radio hears
    /// instead of serving the interface (`survey.rs`, a diagnostic).
    survey: bool,
}

impl Args {
    fn from_service() -> Args {
        let mut buffer = [0u8; 128];
        let len = sys::service_args(&mut buffer).min(buffer.len());
        let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
        let value = |key: &str| {
            text.split_whitespace()
                .find_map(|part| part.strip_prefix(key))
        };
        Args {
            dev: value("dev=").and_then(|id| id.parse().ok()),
            ifname: value("ifname=")
                .filter(|name| devmatch::valid_ifname(name))
                .unwrap_or("wlan0")
                .to_string(),
            survey: value("mode=").or(BUILD_MODE) == Some("survey"),
        }
    }
}

fn failure(step: &'static str, detail: String) -> Failure {
    Failure { step, detail }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match sys::cred_get(None) {
        Ok(cred) => trace::event("CRED", &[("uid", &cred.uid), ("caps", &cred.caps)]),
        Err(errno) => trace::event("CRED", &[("errno", &errno)]),
    }
    let args = Args::from_service();
    let found = match device::find(args.dev) {
        Ok(found) => found,
        Err(errno) => {
            // Nothing is touched yet: let `init` try again.
            trace::fail("list", format_args!("errno={errno}"));
            sys::exit(1)
        }
    };
    let Some((row, model)) = found else {
        // Nothing to drive will become drivable by a restart: say so once
        // and exit cleanly (`OnFailure` does not restart a clean exit).
        trace::fail(
            "no-device",
            format_args!("no RTL8822CE (dev={:?})", args.dev),
        );
        sys::exit(0)
    };
    trace::event(
        "UP",
        &[
            ("dev", &row.id),
            ("ifname", &args.ifname.as_str()),
            ("model", &model),
        ],
    );
    let (claimed, regs) = match attach(row) {
        Ok(attached) => attached,
        Err(why) => {
            // Not mapped yet (a claim a reloaded driver still holds is
            // `EBUSY` for a moment): exit for `init` to retry.
            trace::fail(why.step, format_args!("{}", why.detail));
            sys::exit(1)
        }
    };
    let (chip, mac, irq) = match run(claimed, regs) {
        Ok(up) => up,
        Err(why) => {
            trace::fail(why.step, format_args!("{}", why.detail));
            park()
        }
    };
    if args.survey {
        survey(Radio::survey(chip, mac, sys::clock()), irq)
    }
    // The station side: `os.lazy.net.wifi.hw/<if>` for `wlanmd` and
    // `os.lazy.net.nic/<if>` for `netd`, served by the shared soft-MAC.
    let uid = sys::cred_get(None).map_or(netpolicy::WIFI_UID, |cred| cred.uid);
    // `run` drops the radio on its way out, which powers the chip off.
    let why = match user::wifi::run(hardware::Rtw::new(chip, mac, irq), &args.ifname, uid) {
        Ok(()) => String::from("the service loop ended"),
        Err(why) => why,
    };
    trace::fail("serve", format_args!("{why}"));
    // A service failure (not the chip's): exit so `init` restarts the driver.
    sys::exit(1)
}

/// The claim's interrupt channel and device handle (`None`: polling).
type Irq = Option<(Endpoint, u64)>;

fn step_failure((step, errno): device::StepError) -> Failure {
    failure(step, format!("errno={errno}"))
}

/// Claim the card (memory decode only) and map its register BAR.
fn attach(row: Row) -> Result<(device::Claimed, bus::Bar), Failure> {
    let claimed = device::claim(row).map_err(step_failure)?;
    if let Ok((revision, vendor, device)) = device::identity(&claimed) {
        let subsystem = format!("{vendor:04x}:{device:04x}");
        trace::event(
            "PCI",
            &[
                ("vendor", &format!("{:04x}", row.vendor).as_str()),
                ("device", &format!("{:04x}", row.device).as_str()),
                ("rev", &revision),
                ("subsystem", &subsystem.as_str()),
                ("bar2_bytes", &row.bar_len[device::BAR]),
            ],
        );
    }
    let (base, len) = device::map(&claimed).map_err(step_failure)?;
    // SAFETY: `device::map` mapped the whole BAR, `len` bytes, uncached; the
    // mapping lives until the claim ends with this task.
    let regs = unsafe { bus::Bar::new(base, len) };
    Ok((claimed, regs))
}

/// Identify, quiesce, arm, load the firmware and bring the radio up.
fn run(
    mut claimed: device::Claimed,
    regs: bus::Bar,
) -> Result<(radio::Chip, [u8; 6], Irq), Failure> {
    let identity = chip::identify(&regs);
    identity.report();
    chip::dump("boot", &regs, chip::SYSTEM_REGS);
    if identity.bus_dead() {
        return Err(failure(
            "chip",
            format!("REG_SYS_CFG1 reads {:#010x}", identity.sys_cfg1),
        ));
    }
    // Before bus mastering: a previous driver's rings may still point at
    // memory it no longer owns.
    let quiet = rtw88::pci::quiesce(&regs);
    trace::event(
        "QUIESCE",
        &[
            ("was_running", &quiet.was_running()),
            ("cr_before", &trace::Hex32(quiet.cr_before.into())),
            ("cr_after", &trace::Hex32(quiet.cr_after.into())),
        ],
    );
    let mode = device::arm(&mut claimed).map_err(step_failure)?;
    trace::event("IRQ", &[("mode", &device::mode_name(mode))]);
    let image = firmware::load().map_err(|why| {
        failure(
            "firmware",
            format!("{}/{}: {why}", fhs::FIRMWARE, rtw88::FIRMWARE),
        )
    })?;
    firmware::report(&image);
    // The rings are allocated and programmed next, in memory this task owns.
    device::enable_bus_master(&claimed).map_err(step_failure)?;
    let bus = radio::Bus {
        regs,
        dma: bus::Dma::new(claimed.handle),
        delay_us: bus::delay_us,
    };
    let (chip, mac) = radio::bring_up(bus, image).map_err(|(regs, why)| {
        radio::failure_dump(&regs);
        why
    })?;
    let irq = claimed
        .irq_channel
        .filter(|_| mode.is_some())
        .map(|channel| (channel, claimed.handle));
    Ok((chip, mac, irq))
}

/// Wait on the interrupt channel (`irq`: the claim's channel and device
/// handle, `None` when polling) until the radio's next deadline, handle
/// what arrived, and let the radio hop channels; never busy.
fn survey(mut radio: Radio, irq: Irq) -> ! {
    trace::event("IDLE", &[("serving", &"survey")]);
    let mut buffer = [0u8; 256];
    loop {
        let deadline = radio.deadline().min(sys::clock() + IDLE_TICKS);
        match irq {
            None => {
                let _ = sys::wait(deadline);
            }
            Some((channel, handle)) => {
                let _ = wait::wait_any(&[channel], 0, Some(deadline));
                while let Ok(Some(message)) = channel.poll_recv_with(&mut buffer) {
                    if device::is_interrupt(&message) {
                        radio.interrupt();
                        let _ = user::dev::irq_ack(handle);
                    }
                }
            }
        }
        radio.tick(sys::clock());
    }
}

/// Park for good after a failure, the claim (if any) held.
fn park() -> ! {
    loop {
        let _ = sys::wait(sys::clock() + IDLE_TICKS);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    trace::fail("panic", format_args!("{}", info.message()));
    sys::exit(1)
}
