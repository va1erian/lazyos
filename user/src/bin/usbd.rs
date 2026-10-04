//! `usbd` (`/system/bin/usbd`): the USB driver (`docs/usb-hid-plan.md`, U2;
//! real hardware: `docs/real-pc-boot-plan.md` H3).
//!
//! An ordinary ring-3 program, like `sndd`: it claims **every** xHCI
//! controller through the device syscall (23) (a desktop has a chipset one
//! and often a CPU-side or add-in one), takes each from the BIOS, maps BAR
//! 0, allocates DMA memory for the rings and contexts, and drives them with
//! `libs/xhci` by polling. Each connected port, on the root hub or on an
//! external hub, is reset and its device addressed and bound to its class
//! (`usbd/class.rs`): a boot keyboard or mouse (any interface of a composite
//! device) or a report-protocol pointer is published onto the raw input bus
//! as a kernel input source of its class, so `inputd` sees it exactly like
//! the PS/2 devices; a hub has its ports powered and watched.
//!
//! It holds `CAP_DEV_CLAIM`, `CAP_INPUT_SOURCE` and `CAP_BLOCK_PROVIDER`
//! and nothing else (`init` runs it as `_usb`); it cannot read the bus and
//! the kernel stamps its records with device ids of their own. A USB stick
//! (`usbd/msc.rs`) is served to the kernel as a block device (syscall 33,
//! docs/architecture/usb-storage.md).
//!
//! Serial evidence: `USBD:XHCI hc=<n>` (a controller up, with its BIOS
//! handoff and port protocols), `USBD:PORT port=<hc>-<root>[.<hub port>...]`
//! (a device), `USBD:DESC:*` (its descriptors, hex), `USBD:HUB` (a hub),
//! `USBD:HID:KBD` / `USBD:HID:MOUSE` / `USBD:HID:TABLET` (configured and
//! publishing), `USBD:READY`, `USBD:DETACH` (unplugged: keys and buttons
//! released, slot disabled, children first); with `trace=1`, one `USBD:KEY`
//! line per key edge. Devices come and go at runtime (U3): a port change
//! attaches or detaches, and a detached device's DMA memory serves its
//! slot's next device (`regions=` counts allocations per controller).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::sys;

#[path = "usbd/bus.rs"]
mod bus;
#[path = "usbd/class.rs"]
mod class;
#[path = "usbd/device.rs"]
mod device;
#[path = "usbd/hc.rs"]
mod hc;
#[path = "usbd/hid.rs"]
mod hid;
#[path = "usbd/hub.rs"]
mod hub;
#[path = "usbd/irq.rs"]
mod irq;
#[path = "usbd/mem.rs"]
mod mem;
#[path = "usbd/msc.rs"]
mod msc;
#[path = "usbd/msc_link.rs"]
mod msc_link;
#[path = "usbd/pipe.rs"]
mod pipe;
#[path = "usbd/port.rs"]
mod port;

use bus::Controller;
use hc::Hc;

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

/// How the driver was started.
pub(crate) struct Settings {
    /// Echo every report and key edge on serial (test images only).
    pub(crate) trace: bool,
    /// The restart test (`LAZYOS_USB_CRASH_TEST`): exit after publishing the
    /// first key press, holding it.
    pub(crate) crash_on_key: bool,
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("usbd: USB driver\n");
    // The identity the kernel stamped on this task: `_usb` with only
    // `CAP_DEV_CLAIM | CAP_INPUT_SOURCE | CAP_BLOCK_PROVIDER` under `init`.
    let mut cred = sys::Cred::default();
    match sys::cred_get(None, &mut cred) {
        Ok(()) => sys::write_str(&format!(
            "USBD:CRED uid={} caps={:#x}\n",
            cred.uid, cred.caps
        )),
        Err(errno) => sys::write_str(&format!("USBD:CRED unavailable (errno {errno})\n")),
    }
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

/// The service arguments `init` passed: `trace=1`, and the `attempt=<n>` it
/// appends to every spawn.
struct Args {
    trace: bool,
    attempt: u32,
}

impl Args {
    fn from_service() -> Args {
        let mut buf = [0u8; 64];
        let len = sys::service_args(&mut buf);
        let text = core::str::from_utf8(&buf[..len.min(buf.len())]).unwrap_or("");
        let mut args = Args {
            trace: false,
            attempt: 1,
        };
        for arg in text.split_whitespace() {
            match arg.split_once('=') {
                Some(("trace", "1")) => args.trace = true,
                Some(("attempt", n)) => args.attempt = n.parse().unwrap_or(1),
                _ => {}
            }
        }
        args
    }
}

fn run() -> Result<(), Error> {
    let args = Args::from_service();
    let rows = hc::find_all()?;
    if rows.is_empty() {
        return Err(Error::NoController);
    }
    let settings = Settings {
        trace: args.trace,
        crash_on_key: cfg!(lazyos_usb_crash_test) && args.attempt == 1,
    };
    // One controller failing (a claim refused, a reset that never ends)
    // must not cost the keyboard on another.
    let mut controllers = Vec::new();
    let mut last_error = None;
    for (index, row) in rows.iter().enumerate() {
        match Hc::open(row, index) {
            Ok(hc) => {
                report(&hc, row);
                controllers.push(Controller::new(hc));
            }
            Err(error) => {
                sys::write_str(&format!(
                    "USBD:XHCI:FAIL hc={index} vendor={:#06x} device={:#06x} {error}\n",
                    row.vendor, row.device
                ));
                last_error = Some(error);
            }
        }
    }
    if controllers.is_empty() {
        return Err(last_error.unwrap_or(Error::NoController));
    }
    if args.trace {
        // Harness evidence (issue #481): `_usb` cannot claim another class.
        // After the claims: the probe skips the classes this task owns.
        user::dev::inspect::cross_class_probe("usb");
    }
    for controller in &mut controllers {
        controller.scan(&settings);
    }
    // The kernel's late `/home` mount stops waiting for a stick once every
    // stick present at boot was looked at.
    let _ = sys::storage_scanned();
    let devices: usize = controllers.iter().map(Controller::devices).sum();
    sys::write_str(&format!(
        "USBD:READY devices={devices} controllers={}\n",
        controllers.len()
    ));
    let mut irq_buf = alloc::vec![0u8; 256];
    loop {
        // Acknowledge interrupts first: the line stays masked until then,
        // and an event landing after the acknowledgement interrupts again.
        irq::service(&mut controllers, &mut irq_buf);
        let mut busy = false;
        for controller in &mut controllers {
            busy |= controller.poll(&settings);
            busy |= controller.serve_storage();
        }
        // A live stick's request queue is not a Messenger endpoint, so with
        // one plugged in the idle wait stays its one-tick serve.
        if !busy && !controllers.iter_mut().any(Controller::wait_storage) {
            irq::park(&controllers);
        }
    }
}

/// The `USBD:XHCI` line: what the controller is and how it came up.
fn report(hc: &Hc, row: &user::dev::Row) {
    let info = hc.info;
    let mut usb2 = 0;
    let mut usb3 = 0;
    for port in 1..=info.ports {
        match hc.ports.major(port) {
            Some(2) => usb2 += 1,
            Some(3) => usb3 += 1,
            _ => {}
        }
    }
    sys::write_str(&format!(
        "USBD:XHCI hc={} vendor={:#06x} device={:#06x} version={:#x} ports={} usb2={usb2} usb3={usb3} slots={} scratchpads={} csz64={} ac64={} ppc={} handoff={:?}\n",
        hc.index,
        row.vendor,
        row.device,
        info.version,
        info.ports,
        info.slots,
        info.scratchpads,
        info.context_64,
        info.addressing_64,
        info.port_power,
        info.handoff
    ));
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    sys::write_str(&format!("USBD:PANIC {info}\n"));
    sys::exit(2)
}
