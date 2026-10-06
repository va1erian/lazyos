//! How the driver reaches one virtio-blk function: the legacy (0.9.5) I/O
//! window, or the modern (1.x) transport through the virtio PCI capabilities
//! and a memory BAR (issue #497, driver-plan D7). Both drive the same split
//! queue in the same static memory ([`super::queue`]); only reset, the queue
//! address registers, the doorbell and the device configuration differ.

use virtio::transport::{Kick, Transport};

use super::io;
use super::modern;
use super::ring::Ring;
use super::Slot;

pub(in crate::block) enum Regs {
    /// The legacy I/O BAR at this port base.
    Legacy { io: u16 },
    /// The modern transport and the doorbell of queue 0 (set by a reset).
    Modern {
        transport: Transport,
        kick: Option<Kick>,
    },
}

impl Regs {
    /// Reset the device (it then touches no guest memory), set queue 0 up in
    /// `slot`'s zeroed queue memory, and go. Returns the fresh ring.
    ///
    /// # Safety
    /// `self` must be the device `slot` drives (or is about to), and nothing
    /// else may use the slot's queue meanwhile.
    pub(super) unsafe fn reset(&mut self, slot: &Slot) -> Option<Ring> {
        match self {
            // SAFETY: forwarded from this function's contract.
            Regs::Legacy { io } => unsafe { io::reset(slot, *io) },
            Regs::Modern { transport, kick } => {
                // SAFETY: as above.
                let (ring, new_kick) = unsafe { modern::reset(slot, transport) }?;
                *kick = Some(new_kick);
                Some(ring)
            }
        }
    }

    /// Tell the device queue 0 has new requests.
    pub(super) fn notify(&self) {
        match self {
            Regs::Legacy { io } => io::out16(io + io::QUEUE_NOTIFY, 0),
            Regs::Modern {
                transport,
                kick: Some(kick),
            } => transport.notify(*kick),
            // Never reset: nothing can be queued.
            Regs::Modern { kick: None, .. } => {}
        }
    }

    /// The capacity in 512-byte sectors from the device configuration.
    pub(super) fn capacity(&self) -> u64 {
        match self {
            Regs::Legacy { io } => io::capacity(*io),
            Regs::Modern { transport, .. } => {
                let half = |offset| transport.device_config(offset, 4).unwrap_or(0);
                u64::from(half(4)) << 32 | u64::from(half(0))
            }
        }
    }

    /// The device status register, for failure reports.
    pub(in crate::block) fn status(&self) -> u8 {
        match self {
            Regs::Legacy { io } => io::status(*io),
            Regs::Modern { transport, .. } => transport.status(),
        }
    }

    /// How the device is reached, for the log.
    pub(in crate::block) fn describe(&self) -> alloc::string::String {
        match self {
            Regs::Legacy { io } => alloc::format!("legacy io {io:#x}"),
            Regs::Modern { .. } => alloc::string::String::from("modern"),
        }
    }
}
