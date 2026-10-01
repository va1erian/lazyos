//! Device classes and the ACL identifiers the `dev_*` syscall authorizes
//! against (issue #240, driver-plan section 3.5).
//!
//! The kernel does not know what a NIC is, but the policy needs a stable name
//! per device *class* so "label `net-driver` may claim class net" cannot be
//! stretched to audio or storage. `claim` resolves the device first, maps its
//! PCI class code to one of the names below, and authorizes against the
//! class-specific interface id `os.kernel.dev.<class>`.

use crate::ipc::topics::{fnv1a32, fnv1a64};

use super::DeviceInfo;

/// Interface id of the generic `os.kernel.dev` interface (`list`).
pub const DEV_INTERFACE: u64 = fnv1a64("os.kernel.dev");

/// Method ids on the device interfaces, hashed like every Messenger method
/// (`tools/midlc`), so a compiled policy can name them.
pub mod method {
    use super::fnv1a32;
    /// List discoverable devices (on `os.kernel.dev`).
    pub const LIST: u32 = fnv1a32("list");
    /// Claim a device of the class (on `os.kernel.dev.<class>`).
    pub const CLAIM: u32 = fnv1a32("claim");
    /// Map BARs / use port I/O (grants `MMIO` and `PIO`).
    pub const MAP: u32 = fnv1a32("map");
    /// Bus-master DMA (grants `DMA`).
    pub const DMA: u32 = fnv1a32("dma");
    /// Audit-only: a claim was released.
    pub const RELEASE: u32 = fnv1a32("release");
    /// Audit-only: an IRQ ack deadline expired.
    pub const IRQ_TIMEOUT: u32 = fnv1a32("irq_timeout");
    /// The one-way interrupt notification the kernel posts to a claimant's
    /// endpoint (on `os.kernel.dev`, body fields: 1 device id, 2 irq index,
    /// 3 claim generation, all `u32`).
    pub const IRQ: u32 = fnv1a32("irq");
}

/// One class: its short name, the full interface name and its hashed id.
pub struct Class {
    pub name: &'static str,
    pub interface: &'static str,
    pub interface_id: u64,
}

macro_rules! class {
    ($name:literal) => {
        Class {
            name: $name,
            interface: concat!("os.kernel.dev.", $name),
            interface_id: fnv1a64(concat!("os.kernel.dev.", $name)),
        }
    };
}

pub static STORAGE: Class = class!("storage");
pub static NET: Class = class!("net");
pub static DISPLAY: Class = class!("display");
pub static AUDIO: Class = class!("audio");
pub static MULTIMEDIA: Class = class!("multimedia");
pub static BRIDGE: Class = class!("bridge");
pub static SERIAL: Class = class!("serial");
/// USB host controllers (xHCI, EHCI, UHCI, OHCI): `usbd`'s class, kept apart
/// from other serial-bus functions so its rule grants exactly the controllers
/// (`docs/usb-hid-plan.md` U5).
pub static USB: Class = class!("usb");
pub static SYSTEM: Class = class!("system");
pub static OTHER: Class = class!("other");

/// PCI base-class code for bridges (also used by the DMA-capability rule).
pub const PCI_CLASS_BRIDGE: u8 = 0x06;

/// The class of `info`, from its PCI base class / subclass. Unknown codes map
/// to `other`, which policy can leave unauthorized.
pub fn class_of(info: &DeviceInfo) -> &'static Class {
    match (info.class, info.subclass) {
        (0x01, _) => &STORAGE,
        (0x02, _) => &NET,
        (0x03, _) => &DISPLAY,
        (0x04, 0x01 | 0x03) => &AUDIO,
        (0x04, _) => &MULTIMEDIA,
        (PCI_CLASS_BRIDGE, _) => &BRIDGE,
        (0x08, _) => &SYSTEM,
        (0x0C, 0x03) => &USB,
        (0x0C, _) => &SERIAL,
        _ => &OTHER,
    }
}
