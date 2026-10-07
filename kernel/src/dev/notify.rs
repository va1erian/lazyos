//! The kernel-to-driver interrupt notification (issue #240): one one-way
//! Messenger message from the kernel identity per delivered interrupt, the
//! same for an INTx line and an MSI vector, and the audit record of a
//! claimant that let its ack deadline pass.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use libmessenger::{flags, Encoder, Header, Parcel, VERSION};

use crate::ipc::channels::{self, Error as ChannelError};

use super::class::{method, DEV_INTERFACE};
use super::intx::TIMEOUTS;
use super::{report, DeviceId};

pub(super) fn record_timeout(id: DeviceId, owner: usize) {
    TIMEOUTS.fetch_add(1, Ordering::Relaxed);
    let info = super::table().lock().get(id);
    if let Some(info) = info {
        report::record(
            owner,
            &info,
            method::IRQ_TIMEOUT,
            false,
            report::reason::IRQ_TIMEOUT,
        );
    }
}

/// The wire form of an interrupt notification.
fn encode_irq(dev: u16, generation: u32) -> Option<Vec<u8>> {
    let mut body = Encoder::new();
    body.u32(1, u32::from(dev)).ok()?;
    body.u32(2, 0).ok()?;
    body.u32(3, generation).ok()?;
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: DEV_INTERFACE,
            method: method::IRQ,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel.encode(&mut bytes).ok()?;
    Some(bytes)
}

pub(super) fn post_irq(
    dev: u16,
    generation: u32,
    channel: u64,
    side: usize,
) -> Result<(), ChannelError> {
    let bytes = encode_irq(dev, generation).ok_or(ChannelError::BadParcel)?;
    channels::post_from_kernel(channel, side, &bytes)
}
