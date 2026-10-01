//! `usbd` (`USBD.ELF`): the USB HID driver (`docs/usb-hid-plan.md`, U2).
//!
//! An ordinary ring-3 program, like `sndd`: it claims the xHCI controller
//! through the device syscall (23), maps BAR 0, allocates DMA memory for the
//! rings and contexts, and drives the controller with `libs/xhci` by polling.
//! Each connected root port is reset and its device addressed; a boot
//! keyboard or mouse is configured (`libs/usbhid` parses every descriptor and
//! report) and published onto the raw input bus as a kernel input source of
//! its class, so `inputd` sees it exactly like the PS/2 devices.
//!
//! It holds `CAP_DEV_CLAIM` and `CAP_INPUT_SOURCE` and nothing else (`init`
//! runs it as `_usb`); it cannot read the bus and the kernel stamps its
//! records with device ids of their own.
//!
//! Serial evidence: `USBD:XHCI` (controller up), `USBD:PORT` (a device),
//! `USBD:DESC:*` (its descriptors, hex), `USBD:HID:KBD` / `USBD:HID:MOUSE`
//! (configured and publishing), `USBD:READY`; with `trace=1`, one
//! `USBD:KEY` line per key edge.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use usbhid::desc::Protocol;
use user::sys;

#[path = "usbd/device.rs"]
mod device;
#[path = "usbd/hc.rs"]
mod hc;
#[path = "usbd/hid.rs"]
mod hid;
#[path = "usbd/mem.rs"]
mod mem;

use device::{Device, MAX_REPORT};
use hc::Hc;
use hid::Hid;

/// Why the driver stopped, or a device was given up on.
pub(crate) enum Error {
    NoController,
    Bar,
    PageSize,
    NoPorts,
    Dev(i64),
    Source(i64),
    Xhci(xhci::Error),
    Timeout(&'static str),
    /// A command or request (by TRB type or request code) failed with this
    /// completion code.
    Completion(u8, u8),
    Descriptor(&'static str),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NoController => f.write_str("no xHCI controller"),
            Error::Bar => f.write_str("BAR 0 is not a usable memory BAR"),
            Error::PageSize => f.write_str("controller does not support 4 KiB pages"),
            Error::NoPorts => f.write_str("controller reports no slots or ports"),
            Error::Dev(code) => write!(f, "device syscall failed ({code})"),
            Error::Source(code) => write!(f, "input source refused ({code})"),
            Error::Xhci(error) => write!(f, "ring or context error ({error:?})"),
            Error::Timeout(what) => write!(f, "timed out waiting for {what}"),
            Error::Completion(what, code) => {
                write!(f, "{what:#x} failed with completion code {code}")
            }
            Error::Descriptor(what) => write!(f, "bad descriptor: {what}"),
        }
    }
}

/// A device and the source it publishes through.
struct Bound {
    device: Device,
    hid: Hid,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("usbd: USB HID driver\n");
    match run() {
        Ok(()) => sys::exit(0),
        Err(Error::NoController) => {
            // Not an error: this machine has no xHCI controller.
            sys::write_str("USBD:XHCI:NONE\n");
            sys::exit(0)
        }
        Err(error) => {
            sys::write_str(&format!("USBD:FATAL {error}\n"));
            sys::exit(1)
        }
    }
}

fn trace_enabled() -> bool {
    let mut buf = [0u8; 64];
    let len = sys::service_args(&mut buf);
    let args = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
    args.split_whitespace().any(|arg| arg == "trace=1")
}

fn run() -> Result<(), Error> {
    let trace = trace_enabled();
    let mut hc = Hc::open()?;
    let info = hc.info;
    sys::write_str(&format!(
        "USBD:XHCI version={:#x} ports={} slots={} scratchpads={} csz64={}\n",
        info.version, info.ports, info.slots, info.scratchpads, info.context_64
    ));
    let mut bound: Vec<Bound> = Vec::new();
    for port in 1..=info.ports {
        if let Some(found) = attach_port(&mut hc, port) {
            bound.push(found);
        }
    }
    sys::write_str(&format!("USBD:READY devices={}\n", bound.len()));
    let mut report = [0u8; MAX_REPORT];
    loop {
        let Some(event) = hc.next_event() else {
            hc::nap();
            continue;
        };
        let Some(index) = bound.iter().position(|b| b.device.owns(&event)) else {
            // Port changes (hot-plug) are U3; anything else is ignored.
            continue;
        };
        let entry = &mut bound[index];
        match entry.device.take_report(&event, &mut report) {
            Some(len) => {
                entry.hid.report(&report[..len], trace);
                if entry.device.queue_report(&mut hc).is_ok() {
                    continue;
                }
            }
            None => sys::write_str(&format!(
                "USBD:PORT:FAIL port={} transfer code={}\n",
                entry.device.port,
                event.completion_code()
            )),
        }
        // The pipe failed: release what the device held and drop it.
        let gone = bound.swap_remove(index);
        gone.hid.close();
    }
}

/// Reset `port`, address its device and bind it if it is a boot keyboard or
/// mouse. Failures are reported per port and never stop the driver.
fn attach_port(hc: &mut Hc, port: u8) -> Option<Bound> {
    let speed = device::reset_port(hc, port)?;
    sys::write_str(&format!("USBD:PORT port={port} speed={speed:?}\n"));
    let device = match Device::attach(hc, port, speed) {
        Ok(Some(device)) => device,
        Ok(None) => {
            sys::write_str(&format!(
                "USBD:PORT:SKIP port={port} not a boot HID device\n"
            ));
            return None;
        }
        Err(error) => {
            sys::write_str(&format!("USBD:PORT:FAIL port={port} {error}\n"));
            return None;
        }
    };
    let hid = match Hid::new(device.hid.protocol) {
        Ok(hid) => hid,
        Err(error) => {
            sys::write_str(&format!("USBD:PORT:FAIL port={port} {error}\n"));
            return None;
        }
    };
    let what = match device.hid.protocol {
        Protocol::Keyboard => "KBD",
        _ => "MOUSE",
    };
    sys::write_str(&format!(
        "USBD:HID:{what} port={port} slot={} vendor={:#06x} product={:#06x} interface={} dci={}\n",
        device.slot,
        device.descriptor.vendor,
        device.descriptor.product,
        device.hid.number,
        device.dci
    ));
    Some(Bound { device, hid })
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    sys::write_str(&format!("USBD:PANIC {info}\n"));
    sys::exit(2)
}
