//! Find, claim and bring up the virtio-net PCI function: enable decode and bus
//! mastering, parse the virtio capabilities from config space, map the BARs
//! they point into and wrap the result in a [`Transport`].

use core::ptr;

use user::dev::{self, Row};
use user::messenger::Endpoint;
use virtio::caps::{self, Location};
use virtio::regs::{pci_device_id, PCI_VENDOR};
use virtio::transport::Transport;
use virtio_net::DEVICE_TYPE;

use super::error::Error;

/// PCI command register offset and the bits the driver sets.
const COMMAND: u64 = 4;
const COMMAND_MEMORY: u64 = 1 << 1;
const COMMAND_BUS_MASTER: u64 = 1 << 2;
const COMMAND_INTX_DISABLE: u64 = 1 << 10;

/// QEMU's default `virtio-net-pci` is the *transitional* function (legacy and
/// modern interfaces both present); with `disable-legacy=on` it is `1041`.
/// Either way the driver uses only the modern interface, through the virtio
/// capability list.
const TRANSITIONAL_DEVICE_ID: u16 = 0x1000;

/// A claimed, initialised-up-to-transport device.
pub(super) struct Claimed {
    pub(super) handle: u64,
    pub(super) transport: Transport,
    /// Whether the kernel delivers this device's interrupt messages to the
    /// service endpoint (the line is routable and armed); `false` means the
    /// driver polls alone.
    pub(super) irq: bool,
}

/// The first virtio-net function in the device list.
fn find() -> Result<Row, Error> {
    let mut rows = [[0u64; dev::ROW_WORDS]; dev::MAX_ROWS];
    let total = dev::list(&mut rows).map_err(Error::Dev)?;
    rows.iter()
        .take(total.min(rows.len()))
        .map(Row::from_words)
        .find(|row| {
            row.flags & dev::row_flag::PCI != 0
                && row.vendor == PCI_VENDOR
                && (row.device == pci_device_id(DEVICE_TYPE)
                    || row.device == TRANSITIONAL_DEVICE_ID)
        })
        .ok_or(Error::NoDevice)
}

/// Map `location`'s BAR (once) and return a pointer to the structure inside
/// it. The structure must lie fully inside a memory BAR of the row.
fn locate(
    handle: u64,
    row: &Row,
    bases: &mut [*mut u8; 6],
    location: Location,
) -> Result<*mut u8, Error> {
    let bar = usize::from(location.bar);
    let end = u64::from(location.end().ok_or(Error::Range)?);
    // Present (bit 0) and memory (bit 1 clear), and big enough.
    if bar >= 6 || row.bar_meta[bar] & 0b11 != 0b01 || end > row.bar_len[bar] {
        return Err(Error::Range);
    }
    if bases[bar].is_null() {
        bases[bar] = dev::map_bar(handle, bar as u64).map_err(Error::Dev)?;
    }
    // SAFETY: `end <= bar_len`, and the kernel mapped the whole BAR.
    Ok(unsafe { bases[bar].add(location.offset as usize) })
}

/// Claim the device and build its transport. `server` is the driver's own
/// service endpoint: the kernel posts interrupt messages into its inbox, so
/// client calls and interrupts arrive in the one receive loop. The line is
/// shared (with the polled virtio-blk on both QEMU machine types), so the
/// claim opts in to sharing. If the kernel refuses the endpoint, or the
/// settings say `irq_mode=poll`, the claim is made without one and the driver
/// polls, which is always correct.
pub(super) fn open(server: &Endpoint, allow_irq: bool) -> Result<Claimed, Error> {
    let row = find()?;
    let claimed_with_irq = if allow_irq {
        dev::claim(row.id, Some(server.handle()), true).ok()
    } else {
        None
    };
    let (handle, with_irq) = match claimed_with_irq {
        Some(handle) => (handle, true),
        None => (dev::claim(row.id, None, false).map_err(Error::Dev)?, false),
    };
    let command = dev::cfg_read(handle, COMMAND, 2).map_err(Error::Dev)?;
    dev::cfg_write(
        handle,
        COMMAND,
        2,
        u64::from(command) | COMMAND_MEMORY | COMMAND_BUS_MASTER,
    )
    .map_err(Error::Dev)?;

    let caps = caps::parse(|offset, width| {
        dev::cfg_read(handle, u64::from(offset), u64::from(width)).unwrap_or(0)
    })?;

    let mut bases = [ptr::null_mut::<u8>(); 6];
    let common = locate(handle, &row, &mut bases, caps.common)?;
    let notify = locate(handle, &row, &mut bases, caps.notify)?;
    let isr = locate(handle, &row, &mut bases, caps.isr)?;
    let (device, device_len) = match caps.device {
        Some(location) => (locate(handle, &row, &mut bases, location)?, location.length),
        None => (ptr::null_mut(), 0),
    };
    if caps.common.length < virtio::regs::common::LEN as u32 {
        return Err(Error::Range);
    }
    // SAFETY: every pointer was bounds-checked against its BAR above and the
    // mappings live until the claim is released with the task.
    let transport = unsafe {
        Transport::new(
            common,
            notify,
            caps.notify.length,
            caps.notify_multiplier,
            isr,
            device,
            device_len,
        )
    };
    // Arm the line last, once the device is fully described: an unroutable
    // line answers ENOSYS and the driver polls. The kernel keeps INTx disabled
    // until the claim is armed, so the command register is written again to
    // let the device assert it.
    let armed = with_irq && dev::irq_enable(handle).is_ok();
    if armed {
        let command = dev::cfg_read(handle, COMMAND, 2).map_err(Error::Dev)?;
        dev::cfg_write(
            handle,
            COMMAND,
            2,
            u64::from(command) & !COMMAND_INTX_DISABLE,
        )
        .map_err(Error::Dev)?;
    }
    Ok(Claimed {
        handle,
        transport,
        irq: armed,
    })
}
