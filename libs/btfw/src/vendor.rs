//! Which firmware loader applies to a controller.
//!
//! Two sources, in the order `btd` has them: the USB ids (before the radio is
//! opened) and the manufacturer in `Read_Local_Version` (once it answers). The
//! USB ids are a hint, since a vendor's id also covers parts that need
//! nothing; the answer that matters is the loader's own first command.

use bthci::ret::LocalVersion;

use crate::ids::REALTEK;

/// Intel's USB vendor id. Every product under it is an Intel radio except the
/// CSR clone `8087:07da` (btusb treats it as a CSR part).
pub const USB_VENDOR_INTEL: u16 = 0x8087;
/// Realtek's USB vendor id (the dongles and the RTL8822CE/RTL8852 cards).
pub const USB_VENDOR_REALTEK: u16 = 0x0bda;
/// Bluetooth SIG company identifiers (Assigned Numbers) as `Read_Local_Version`
/// reports them.
pub const COMPANY_INTEL: u16 = 2;
pub const COMPANY_REALTEK: u16 = 93;

/// Which loader to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    Intel,
    Realtek,
    /// Generic HCI: the controller works as it comes (CSR, Broadcom without
    /// patch RAM needs, QEMU's fake dongles); load nothing.
    Generic,
}

impl Vendor {
    /// From a device's USB vendor and product ids.
    pub fn from_usb(vendor_id: u16, product_id: u16) -> Vendor {
        match (vendor_id, product_id) {
            (USB_VENDOR_INTEL, 0x07da) => Vendor::Generic,
            (USB_VENDOR_INTEL, _) => Vendor::Intel,
            (USB_VENDOR_REALTEK, _) => Vendor::Realtek,
            ids if REALTEK.binary_search(&ids).is_ok() => Vendor::Realtek,
            _ => Vendor::Generic,
        }
    }

    /// From the manufacturer field of `Read_Local_Version`.
    pub fn from_local_version(version: &LocalVersion) -> Vendor {
        match version.manufacturer {
            COMPANY_INTEL => Vendor::Intel,
            COMPANY_REALTEK => Vendor::Realtek,
            _ => Vendor::Generic,
        }
    }

    /// The more specific of the two answers: the manufacturer wins when it
    /// names a vendor (the radio says what it is), the USB ids otherwise.
    pub fn identify(usb: Option<(u16, u16)>, version: Option<&LocalVersion>) -> Vendor {
        let by_version = version.map_or(Vendor::Generic, Vendor::from_local_version);
        match (by_version, usb) {
            (Vendor::Generic, Some((vid, pid))) => Vendor::from_usb(vid, pid),
            (found, _) => found,
        }
    }
}
