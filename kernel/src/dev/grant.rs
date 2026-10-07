//! The grant rule (issue #240, driver-plan D3): which rights a `Device` handle
//! gets.
//!
//! `rights = (resources the device really has) AND (what class policy permits
//! this actor)`. A driver can therefore never hold `MMIO` for a device with no
//! memory BAR, or `DMA` for a bridge, and policy can withhold a family the
//! device has. Rights are fixed at claim time and only ever narrowed.

use crate::ipc::acl;
use crate::ipc::credentials::Cred;
use crate::ipc::handles::rights;

use super::class::{method, Class, PCI_CLASS_BRIDGE};
use super::irq::LINES;
use super::{BarKind, BusId, DeviceInfo};

/// The rights the device's own resources can support.
pub fn resource_rights(info: &DeviceInfo) -> u32 {
    let mut granted = 0;
    if info
        .resources
        .bars()
        .any(|bar| bar.kind == BarKind::Mem && bar.len > 0)
    {
        granted |= rights::DEV_MMIO;
    }
    if info
        .resources
        .bars()
        .any(|bar| bar.kind == BarKind::Io && bar.len > 0)
    {
        granted |= rights::DEV_PIO;
    }
    // A line value of 0xFF is "not connected"; anything else is a real wire,
    // even if the PIC cannot deliver it (then `irq_enable` says ENOSYS).
    // A message capability is an interrupt too (issue #616).
    if info.resources.irq().is_some_and(|irq| irq.line < LINES)
        || (matches!(info.bus, BusId::Pci(_)) && info.resources.message_capable())
    {
        granted |= rights::DEV_IRQ;
    }
    if matches!(info.bus, BusId::Pci(_)) {
        granted |= rights::DEV_CONFIG;
        // Bridges do not master the bus on behalf of a driver.
        if info.class != PCI_CLASS_BRIDGE {
            granted |= rights::DEV_DMA;
        }
    }
    granted
}

/// The rights class policy (the Messenger ACL and the driver class rules of
/// [`super::policy`]) grants `cred` on `class`, given that the `claim`
/// method itself was already allowed: `claim` alone yields `CONFIG` and `IRQ`,
/// `map` adds `MMIO`/`PIO`, `dma` adds `DMA`. These extra lookups use the
/// non-recording evaluator: a driver that is not entitled to DMA is not a
/// denial worth an audit record on every claim.
pub fn policy_rights(cred: &Cred, class: &Class) -> u32 {
    let allowed = |method| {
        !acl::evaluate(cred.authority(), class.interface_id, method).denied()
            && super::policy::allows(cred, class, method)
    };
    let mut granted = rights::DEV_CONFIG | rights::DEV_IRQ;
    if allowed(method::MAP) {
        granted |= rights::DEV_MMIO | rights::DEV_PIO;
    }
    if allowed(method::DMA) {
        granted |= rights::DEV_DMA;
    }
    granted
}
