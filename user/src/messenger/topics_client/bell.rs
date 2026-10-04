//! The subscription doorbell (docs/performance-plan.md P7.2): `Bell` hands
//! the broker one end of a channel, and the broker sends a one-way
//! `os.lazy.messenger.topics.bell.v1` `Ready` on it when the subscription has
//! events nobody is pulling. A subscriber parks on the other end beside its
//! own endpoints instead of polling `NextEvent` on a timer.

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_topics_bell_v1 as bell;
use messenger_generated::os_lazy_messenger_topics_v1 as generated;

use super::super::{Error, Message, Result};
use super::header;

/// The bell's interface id (`os.lazy.messenger.topics.bell.v1`).
pub const BELL_INTERFACE: u64 = bell::INTERFACE_ID;
/// What a `Bell` request must carry: the bell channel end.
pub const BELL_TRANSFERS: messenger_generated::transfers::Transfers = generated::BELL_TRANSFERS;

/// A `Bell` request for subscription `id`, transferring `handle` (the end
/// the broker will send on; the kernel moves it out of the caller's table).
pub fn bell_request(id: u64, handle: u64) -> Result<Parcel> {
    let args = generated::BellArgs { subscription: id };
    let body = generated::encode_bell_args(&args).map_err(Error::Parcel)?;
    let (handles, buffers) =
        generated::encode_bell_transfers(&generated::BellTransfers { bell: handle });
    Ok(Parcel {
        header: header(generated::METHOD_BELL),
        body,
        handles,
        buffers,
    })
}

/// The subscription a `Bell` request names.
pub fn decode_bell_args(parcel: &Parcel) -> Result<u64> {
    Ok(generated::decode_bell_args(&parcel.body)
        .map_err(Error::Parcel)?
        .subscription)
}

/// The one-way `Ready` the broker rings subscription `id`'s bell with.
pub fn ready_note(id: u64) -> Result<Parcel> {
    let body =
        bell::encode_ready_args(&bell::ReadyArgs { subscription: id }).map_err(Error::Parcel)?;
    Ok(Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ONE_WAY,
            interface_id: BELL_INTERFACE,
            method: bell::METHOD_READY,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        ..Parcel::default()
    })
}

/// The subscription a received `Ready` names; `None` for anything else.
pub fn ready_subscription(message: &Message) -> Option<u64> {
    if message.interface_id() != BELL_INTERFACE || message.method() != bell::METHOD_READY {
        return None;
    }
    bell::decode_ready_args(&message.parcel.body)
        .ok()
        .map(|args| args.subscription)
}
