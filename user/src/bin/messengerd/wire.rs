//! The broker's side of `idl/topics.midl` that the generated `dispatch` does
//! not cover: the parked `NextEvent` reply, the `Bell` request that carries a
//! channel end, and the doorbell note. The clients are the generated ones.

use alloc::vec::Vec;
use libmessenger::client::Connection;
use libmessenger::server;
use libmessenger::{flags, Parcel};
use messenger_generated::os_lazy_messenger_topics_bell_v1 as bell;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;
use user::messenger::client::Native;
use user::messenger::topics_client::{Event, INTERFACE};
use user::messenger::{Endpoint, Error, Message, Result};

/// A `NextEvent` reply carrying one event.
pub(super) fn reply_event(event: &Event) -> Result<Parcel> {
    let event = event.clone();
    wire::encode_next_event_reply(&wire::NextEventReply { event })
        .map(|body| server::reply(INTERFACE, wire::METHOD_NEXTEVENT, body))
        .map_err(Error::Parcel)
}

/// The subscription a `NextEvent` request names.
pub(super) fn subscription_id(request: &Parcel) -> Result<u64> {
    wire::decode_next_event_args(&request.body)
        .map(|args| args.subscription)
        .map_err(Error::Parcel)
}

/// The subscription a received `Bell` request names and the end it carried,
/// claimed from the message (the caller owns it from here).
pub(super) fn decode_bell_args(message: &Message) -> Result<(u64, Endpoint)> {
    let args = message.decode(wire::decode_bell_args)?;
    Ok((args.subscription, Endpoint::from_raw(args.bell)))
}

/// The one-way `Ready` the broker rings subscription `id`'s bell with.
pub(super) fn ready_note(id: u64) -> Result<Parcel> {
    let body = bell::encode_ready_args(&bell::ReadyArgs { subscription: id });
    let body = body.map_err(Error::Parcel)?;
    Ok(Connection::<Native>::request(
        bell::INTERFACE_ID,
        bell::METHOD_READY,
        flags::ONE_WAY,
        body,
        Vec::new(),
    ))
}

/// A `Ping` request, for the broker's soak self-call.
pub(super) fn ping_request() -> Parcel {
    Connection::<Native>::request(
        INTERFACE,
        wire::METHOD_PING,
        flags::ALLOW_NESTED,
        Vec::new(),
        Vec::new(),
    )
}
