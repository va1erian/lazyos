//! The virtio-sound function's modern transport: parse the virtio
//! capabilities from config space, map the BARs they point into and wrap the
//! result in a [`Transport`].

use core::ptr;

use user::dev;
use virtio::caps::{self, Location};
use virtio::transport::Transport;

use super::device::{self, Claimed};
use super::error::Error;

/// Map `location`'s BAR (once) and return a pointer to the structure inside
/// it. The structure must lie fully inside a memory BAR of the row.
fn locate(
    claimed: &Claimed,
    bases: &mut [*mut u8; 6],
    location: Location,
) -> Result<*mut u8, Error> {
    let bar = usize::from(location.bar);
    let end = u64::from(location.end().ok_or(Error::Range)?);
    if bases.get(bar).ok_or(Error::Range)?.is_null() {
        bases[bar] = device::map(claimed, bar, end)?;
    } else if end > claimed.row.bar_len[bar] {
        return Err(Error::Range);
    }
    // SAFETY: `end <= bar_len` (checked by `device::map` or above), and the
    // kernel mapped the whole BAR.
    Ok(unsafe { bases[bar].add(location.offset as usize) })
}

/// Build the transport of the claimed function.
pub(super) fn transport(claimed: &Claimed) -> Result<Transport, Error> {
    let handle = claimed.handle;
    let caps = caps::parse(|offset, width| {
        dev::cfg_read(handle, u64::from(offset), u64::from(width)).unwrap_or(0)
    })?;
    let mut bases = [ptr::null_mut::<u8>(); 6];
    let common = locate(claimed, &mut bases, caps.common)?;
    let notify = locate(claimed, &mut bases, caps.notify)?;
    let isr = locate(claimed, &mut bases, caps.isr)?;
    let (device, device_len) = match caps.device {
        Some(location) => (locate(claimed, &mut bases, location)?, location.length),
        None => (ptr::null_mut(), 0),
    };
    if caps.common.length < virtio::regs::common::LEN as u32 {
        return Err(Error::Range);
    }
    // SAFETY: every pointer was bounds-checked against its BAR above and the
    // mappings live until the claim is released with the task.
    Ok(unsafe {
        Transport::new(
            common,
            notify,
            caps.notify.length,
            caps.notify_multiplier,
            isr,
            device,
            device_len,
        )
    })
}
