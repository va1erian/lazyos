//! PCI class codes to ACL device classes (`dev::class::class_of`): the class a
//! driver's claim is authorized against. `usbd` claims the xHCI controller
//! (`0C/03/30`, docs/usb-hid-plan.md U2), which must land in `serial` with
//! every other serial-bus controller and never in a class another driver's
//! rules already cover.

use super::*;
use crate::dev::class::{self, class_of};
use crate::dev::{BusId, DeviceId, DeviceInfo, Resources};

fn info(class_code: u8, subclass: u8, prog_if: u8) -> DeviceInfo {
    DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Platform,
        vendor: 0x1B36,
        device: 0x000D,
        subsystem_vendor: 0,
        subsystem_device: 0,
        class: class_code,
        subclass,
        prog_if,
        revision: 0,
        resources: Resources::empty(),
    }
}

/// xHCI, EHCI, UHCI and OHCI (`0C/03/xx`) are all `serial`, as is any other
/// serial-bus function; audio, network and storage keep their own classes.
pub fn usb_controllers_are_serial() -> Result<(), String> {
    for prog_if in [0x00, 0x10, 0x20, 0x30, 0xFE] {
        let got = class_of(&info(0x0C, 0x03, prog_if)).name;
        check!(
            got == class::SERIAL.name,
            "USB prog-if {prog_if:#x} -> {got}"
        );
    }
    check!(
        class_of(&info(0x0C, 0x05, 0)).name == "serial",
        "SMBus is not serial"
    );
    for (code, sub, want) in [
        (0x04, 0x03, class::AUDIO.name),
        (0x02, 0x00, class::NET.name),
        (0x01, 0x06, class::STORAGE.name),
        (0xFF, 0x00, class::OTHER.name),
    ] {
        let got = class_of(&info(code, sub, 0)).name;
        check!(got == want, "{code:#x}/{sub:#x} -> {got}, want {want}");
    }
    check!(
        class::SERIAL.interface == "os.kernel.dev.serial",
        "serial interface name {}",
        class::SERIAL.interface
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[(
    "dev_class_usb_controllers_are_serial",
    usb_controllers_are_serial,
)];
