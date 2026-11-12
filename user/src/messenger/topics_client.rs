//! The topics broker's domain meaning (issue #92): the delivery contract
//! ([`Qos`]), the limits, and the generated client over the bootstrap channel.
//!
//! The wire is `idl/topics.midl`: [`Client`] is the `midlc`-generated
//! `os.lazy.messenger.topics.v1` client and [`Event`], [`TopicInfo`] and
//! [`SubscriptionStats`] are its generated shapes. A program that publishes or
//! follows topics uses [`crate::central`] (a `Bus` and its `Subscription`s),
//! which is this client plus the payload envelope and the doorbell; only
//! `messengerd`, the broker, frames the other side of the wire.

use libmessenger::client::{CallError, Connection, Deadline};
use messenger_generated::os_lazy_messenger_topics_publish_v1 as publish_scope;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;

use super::client::{resolve_within, Held};
use super::endpoint::{encode, syscall};
use super::{op, Endpoint, Error, MsgArgs, MsgResult, Result};

pub use wire::{Event, Stats as SubscriptionStats, TopicInfo};

/// Well-known broker name; `messengerd` registers it at startup.
pub const NAME: &str = wire::SERVICE_NAME;

/// Topics interface id (`os.lazy.messenger.topics.v1`).
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Kernel mode code for a publish ACL check (`Mode::Publish`).
pub const MODE_PUBLISH: u32 = publish_scope::MODE_PUBLISH;
/// Kernel mode code for a subscribe ACL check (`Mode::Subscribe`).
pub const MODE_SUBSCRIBE: u32 = publish_scope::MODE_SUBSCRIBE;

/// Payload bytes accepted by the broker in one event. Sized well below the
/// 16 KiB call buffer so a `NextEvent` reply always fits.
pub const MAX_PAYLOAD: usize = 8 * 1024;

/// The generated broker client. Every request carries `ALLOW_NESTED`: all
/// clients share the daemon's one endpoint, so a parked `NextEvent` leaves a
/// transaction open while another task publishes.
pub type Client = wire::Client<Held>;

/// Why a [`Client`] call failed.
pub type ClientError = wire::ClientError;

/// A [`Client`] over a broker endpoint this task already resolved.
pub fn client_of(endpoint: Endpoint) -> Client {
    Client::from_connection(Connection::new(Held::nested(endpoint), NAME))
}

/// How long [`connect`] waits for `messengerd` to register: a moment, not
/// the runtime's connect bound. Most callers are daemon loops that try again
/// on their next pass (`if bus.is_none() { bus = Bus::connect().ok() }`) and
/// must not stall for seconds when the broker is late or not in the image.
const CONNECT_BRIEF: Deadline = Deadline::ticks(8);

/// A [`Client`] for a program that may start just before `messengerd`: waits
/// [`CONNECT_BRIEF`] for the name to register.
pub fn connect() -> Result<Client> {
    Ok(client_of(resolve_within(NAME, CONNECT_BRIEF)?))
}

/// A failed broker call as the Messenger error callers match on: the
/// broker's refusal keeps its code, a transport failure its errno.
pub fn lift(error: ClientError) -> Error {
    match error {
        CallError::Transport(errno) => Error::Errno(errno.to_syscall()),
        CallError::Remote { code, .. } => Error::Topics(i64::from(code)),
        CallError::Service(never) => match never {},
    }
}

/// The next event of subscription `id`, waiting `within`: `None` when nothing
/// was queued by then (the broker answers a pull with an empty event only
/// when it has none, and the kernel deadline is the timeout signal).
pub fn next(client: &Client, id: u64, within: Deadline) -> Result<Option<Event>> {
    match client.next_event_within(id, within) {
        Ok(event) if event.topic.is_empty() => Ok(None),
        Ok(event) => Ok(Some(event)),
        Err(error) if error.is_timeout() => Ok(None),
        Err(error) => Err(lift(error)),
    }
}

/// The delivery contract, defined with the broker's tables in `libs/topicsbroker`.
pub use topicsbroker::Qos;

/// Ask the kernel policy engine about `name` for the task in `actor`
/// (the `messengerd` proxy path). `mode` is [`MODE_PUBLISH`] or
/// [`MODE_SUBSCRIBE`]; `txn` is copied into denial audit records.
///
/// `-EACCES` means policy refused a segment; the denial is already in the
/// audit ring.
pub fn authorize(actor: u64, mode: u32, name: &str, txn: u64) -> Result<()> {
    let args = publish_scope::AuthorizeTopicArgs {
        name: alloc::string::String::from(name),
        mode,
        txn,
    };
    let body = publish_scope::encode_authorize_topic_args(&args).map_err(Error::Parcel)?;
    // The kernel op ignores the header; the body carries the query.
    let query = Connection::<Held>::request(
        publish_scope::INTERFACE_ID,
        publish_scope::METHOD_AUTHORIZETOPIC,
        0,
        body,
        alloc::vec::Vec::new(),
    );
    let bytes = encode(&query)?;
    let args = MsgArgs {
        txn_id: actor,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    syscall(op::AUTHORIZE_TOPIC, &args, &mut MsgResult::default())?;
    Ok(())
}
