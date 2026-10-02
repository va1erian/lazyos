//! Class dispatch: what each interface of a newly addressed device becomes.
//!
//! [`bind`] sets the configuration, then walks the interfaces (alternate
//! setting 0) and hands each to the driver of its class code: HID (boot
//! keyboards and mice wherever they sit in a composite device, else a
//! report-protocol pointer), or the hub class for a hub. Each bound
//! interface is a [`Function`]; the controller routes a transfer event to
//! the function owning its endpoint.
//!
//! **Adding a class** (mass storage, for one): add a [`Function`] variant
//! holding the class's state; add an arm in [`bind_interface`] for its
//! class code that sends its class requests through
//! `Device::control_in`/`control_out` and opens its endpoints with
//! `Device::open_pipe` (bulk: the class submits its own transfers) or
//! `Device::open_reports` (interrupt-IN reports `usbd` refills); then give
//! the variant arms in [`Function::owns`], [`Function::on_transfer`] (its
//! pipes' events, for classes driving their own pipes) and
//! [`Function::close`]. `bind` runs one Configure Endpoint for all pipes
//! after every interface was bound.

use alloc::format;
use alloc::vec::Vec;

use usbhid::desc::{Config, HidInterface, Interface, Protocol, CLASS_HID, CLASS_HUB};
use usbhid::report::{self, Pointer};
use user::sys;
use xhci::trb::{request, Trb};

use super::device::{hex, Device, DATA_BYTES};
use super::hc::Hc;
use super::hid::Hid;
use super::hub::HubFn;
use super::Error;

/// One bound interface.
pub(super) enum Function {
    /// A HID interface publishing through its own input source.
    Hid { dci: u8, hid: Hid },
    /// A hub: its status-change pipe and its ports.
    Hub(HubFn),
}

impl Function {
    /// Whether the transfer events of endpoint `dci` are this function's.
    pub(super) fn owns(&self, dci: u8) -> bool {
        match self {
            Function::Hid { dci: own, .. } => *own == dci,
            Function::Hub(hub) => hub.dci == dci,
        }
    }

    /// A transfer event on one of this function's own pipes (classes that
    /// submit their own transfers, e.g. bulk). Report pipes never get here:
    /// `Device::take_report` collects them.
    pub(super) fn on_transfer(
        &mut self,
        _hc: &mut Hc,
        _device: &mut Device,
        _event: &Trb,
    ) -> Result<(), Error> {
        Ok(())
    }

    /// The device is going away: release what it held.
    pub(super) fn close(self, trace: bool) {
        match self {
            Function::Hid { hid, .. } => hid.close(trace),
            Function::Hub(_) => {}
        }
    }
}

/// Configure `device` with `config` and bind every interface this driver
/// drives. Empty when it drives none (the caller leaves the device alone).
pub(super) fn bind(
    hc: &mut Hc,
    device: &mut Device,
    config: &Config,
) -> Result<Vec<Function>, Error> {
    // Class requests address a configured device: configuration first.
    device.control_out(hc, request::set_configuration(config.value))?;
    if device.descriptor.class == CLASS_HUB || config.interfaces().any(|i| i.class == CLASS_HUB) {
        let (mut hub, slot) = HubFn::bind(hc, device, config)?;
        device.configure(hc, Some(slot))?;
        hub.start(hc, device)?;
        return Ok(alloc::vec![Function::Hub(hub)]);
    }
    let mut functions = Vec::new();
    let bound = bind_all(hc, device, config, &mut functions).and_then(|()| {
        if functions.is_empty() {
            Ok(())
        } else {
            device.configure(hc, None)
        }
    });
    match bound {
        Ok(()) => Ok(functions),
        Err(error) => {
            // Sources already registered are closed, not leaked.
            for function in functions {
                function.close(false);
            }
            Err(error)
        }
    }
}

fn bind_all(
    hc: &mut Hc,
    device: &mut Device,
    config: &Config,
    functions: &mut Vec<Function>,
) -> Result<(), Error> {
    for interface in config.interfaces().filter(|i| i.alternate == 0) {
        if let Some(function) = bind_interface(hc, device, interface, false)? {
            functions.push(function);
        }
    }
    if functions.is_empty() {
        // No boot interface: a report-protocol pointer (a tablet) instead.
        for interface in config.interfaces().filter(|i| i.alternate == 0) {
            if let Some(function) = bind_interface(hc, device, interface, true)? {
                functions.push(function);
                break;
            }
        }
    }
    Ok(())
}

/// Bind one interface by its class; `report_protocol` selects the second
/// pass (HID interfaces without a boot protocol).
fn bind_interface(
    hc: &mut Hc,
    device: &mut Device,
    interface: &Interface,
    report_protocol: bool,
) -> Result<Option<Function>, Error> {
    match interface.class {
        CLASS_HID => match interface.hid() {
            Some(hid) if hid.endpoint.is_some() => bind_hid(hc, device, hid, report_protocol),
            _ => Ok(None),
        },
        // New classes go here (see the module documentation).
        _ => Ok(None),
    }
}

/// A boot keyboard or mouse (first pass) or a report-protocol pointer
/// (second pass): its protocol set, its pipe opened, its source registered.
fn bind_hid(
    hc: &mut Hc,
    device: &mut Device,
    hid: HidInterface,
    report_protocol: bool,
) -> Result<Option<Function>, Error> {
    let Some(endpoint) = hid.endpoint else {
        return Ok(None);
    };
    let boot = hid.protocol != Protocol::None;
    if boot == report_protocol {
        return Ok(None);
    }
    let layout = if boot {
        // Many keyboards start in report protocol; boot reports are the
        // fixed 8-byte (keyboard) and 3-byte (mouse) layouts the decoders
        // know. A device that refuses is used as it is.
        if let Err(error) = device.control_out(hc, request::set_protocol(hid.number, true)) {
            note(device, hid.number, "SET_PROTOCOL", &error);
        }
        if hid.protocol == Protocol::Keyboard {
            // Report only on change. Optional in practice: many keyboards
            // stall it, which costs nothing now that a stall is recovered.
            if let Err(error) = device.control_out(hc, request::set_idle(hid.number)) {
                note(device, hid.number, "SET_IDLE", &error);
            }
        }
        None
    } else {
        // Report protocol is the default; SET_PROTOCOL is only for boot
        // devices (QEMU's tablet stalls it). No pointer: not ours.
        match read_report_layout(hc, device, &hid) {
            Ok(layout) => Some(layout),
            Err(_) => return Ok(None),
        }
    };
    let dci = device.open_reports(&endpoint, super::pipe::REPORTS)?;
    let source = Hid::new(hid.protocol, layout)?;
    let what = match hid.protocol {
        Protocol::Keyboard => "KBD",
        _ if source.tablet() => "TABLET",
        _ => "MOUSE",
    };
    sys::write_str(&format!(
        "USBD:HID:{what} port={} slot={} vendor={:#06x} product={:#06x} interface={} dci={} regions={}\n",
        device.name,
        device.slot,
        device.descriptor.vendor,
        device.descriptor.product,
        hid.number,
        dci,
        hc.regions
    ));
    Ok(Some(Function::Hid { dci, hid: source }))
}

fn note(device: &Device, interface: u8, what: &str, error: &Error) {
    sys::write_str(&format!(
        "USBD:HID:NOTE port={} interface={interface} {what} refused ({error}), continuing\n",
        device.name
    ));
}

/// The interface's report descriptor, parsed for a pointer.
fn read_report_layout(
    hc: &mut Hc,
    device: &mut Device,
    hid: &HidInterface,
) -> Result<Pointer, Error> {
    let len = usize::from(hid.report_len);
    if len == 0 || len > DATA_BYTES {
        return Err(Error::Descriptor("report descriptor length"));
    }
    let mut bytes = [0u8; DATA_BYTES];
    let setup = request::get_report_descriptor(hid.number, len as u16);
    device.control_in(hc, setup, &mut bytes[..len])?;
    sys::write_str(&format!(
        "USBD:DESC:REPORT port={} {}\n",
        device.name,
        hex(&bytes[..len])
    ));
    report::parse_pointer(&bytes[..len])
        .map_err(|_| Error::Descriptor("no pointer in the report descriptor"))
}
