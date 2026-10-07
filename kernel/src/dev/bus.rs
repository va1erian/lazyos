//! Bus enumeration (issue #239). The [`Bus`] trait is the seam: today only PCI
//! implements it, but ACPI/platform enumeration can join later without drivers
//! noticing. A bus produces [`DeviceInfo`] rows and drops them into the table.

use super::pci::{self, Address, Function};
use super::{BusId, DeviceId, DeviceInfo, Irq, Msi, MsiX, Resources};

/// A bus that can discover devices.
pub trait Bus {
    /// Short name for logs.
    fn name(&self) -> &'static str;

    /// Enumerate into `table`, reporting how many devices were inserted and
    /// how many did not fit.
    fn enumerate(&self, table: &mut super::table::DeviceTable) -> Enumerated;
}

/// The outcome of one bus enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Enumerated {
    pub inserted: usize,
    /// Functions found but not recorded because the table was full (issue
    /// #274): a driver for one of them would never be probed.
    pub dropped: usize,
}

/// The PCI bus, over legacy config mechanism 1.
pub struct PciBus;

impl Bus for PciBus {
    fn name(&self) -> &'static str {
        "pci"
    }

    fn enumerate(&self, table: &mut super::table::DeviceTable) -> Enumerated {
        let mut found = Enumerated::default();
        pci::for_each(|function| {
            if table.insert(device_info(function)).is_ok() {
                found.inserted += 1;
            } else {
                found.dropped += 1;
            }
        });
        found
    }
}

/// Build a [`DeviceInfo`] row for one enumerated PCI function. The `id` field
/// is a placeholder: the table assigns the real one on insert.
pub fn device_info(function: Function) -> DeviceInfo {
    let address = function.address;
    let header = pci::header_type(address) & 0x7F;
    let mut resources = Resources::empty();
    // Type-0 functions expose up to six BARs; type-1 bridges expose two. Other
    // header types have no BARs we understand yet.
    let bar_count: u8 = match header {
        0 => 6,
        1 => 2,
        _ => 0,
    };
    let mut index = 0u8;
    while index < bar_count {
        if let Some((bar, stride)) = pci::read_bar(address, index) {
            resources.set_bar(bar);
            index += stride;
        } else {
            index += 1;
        }
    }
    // Only a function that wires an INTx pin has an interrupt: bridges and
    // pin-less functions carry whatever the firmware left in the Interrupt Line
    // register (often 0, which is the timer), and it must not be mistaken for a
    // wire.
    if pci::interrupt_pin(address) != 0 {
        resources.set_irq(Irq {
            line: pci::interrupt_line(address),
        });
    }
    if header == 0 {
        message_capabilities(address, &mut resources);
    }
    let (subsystem_vendor, subsystem_device) = pci::subsystem_id(address);
    DeviceInfo {
        id: DeviceId(0),
        bus: BusId::Pci(address),
        vendor: function.vendor,
        device: function.id,
        subsystem_vendor,
        subsystem_device,
        class: pci::class(address),
        subclass: pci::subclass(address),
        prog_if: pci::prog_if(address),
        revision: pci::revision(address),
        resources,
    }
}

/// Capability ids of MSI and MSI-X.
const CAP_MSI: u8 = 0x05;
const CAP_MSIX: u8 = 0x11;

/// Record the function's MSI and MSI-X capabilities (issue #616). Both are
/// hostile input: an MSI-X table that does not lie inside one of the
/// function's own sized memory BARs is dropped, so the kernel never maps or
/// writes outside the device.
fn message_capabilities(address: Address, resources: &mut Resources) {
    pci::for_each_capability(address, |cap| match cap.id {
        CAP_MSI if resources.msi().is_none() => {
            let control = pci::read16(address, cap.offset + 2);
            resources.set_msi(Msi {
                cap: cap.offset,
                is_64: control & (1 << 7) != 0,
                maskable: control & (1 << 8) != 0,
            });
        }
        CAP_MSIX if resources.msix().is_none() => {
            let control = pci::read16(address, cap.offset + 2);
            let table = pci::read32(address, cap.offset + 4);
            if let Some(msix) = check_msix(resources, cap.offset, control, table) {
                resources.set_msix(msix);
            }
        }
        _ => {}
    });
}

/// Decode an MSI-X capability (`control`, the table offset/BIR dword) and
/// keep it only if its table fits a memory BAR of `resources`.
pub(crate) fn check_msix(resources: &Resources, cap: u8, control: u16, table: u32) -> Option<MsiX> {
    let msix = MsiX {
        cap,
        table_size: (control & 0x7FF) + 1,
        table_bar: (table & 0x7) as u8,
        table_offset: table & !0x7,
    };
    let bar = resources.bar(msix.table_bar)?;
    let end = u64::from(msix.table_offset).checked_add(msix.table_len())?;
    (bar.kind == super::BarKind::Mem && bar.base != 0 && end <= bar.len).then_some(msix)
}

/// Convenience for callers that have coordinates rather than a [`Function`]:
/// read the identity and build the row.
pub fn device_info_at(address: Address) -> DeviceInfo {
    device_info(Function {
        address,
        vendor: pci::read16(address, 0),
        id: pci::read16(address, 2),
    })
}
