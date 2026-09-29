//! [`Client`]/[`Subscription`] and the module-level convenience functions
//! for the topics broker.

use alloc::vec::Vec;

use libmessenger::{Encoder, Parcel};

use super::super::endpoint::syscall;
use super::super::{
    create_pair, errno, op, registry, Endpoint, Error, MsgArgs, MsgResult, Result, EXPIRED_DEADLINE,
};
use super::wire::{
    decode_event, decode_stats, decode_topics, error_field, publish_body, request_parcel,
    subscribe_body, subscription_body, u64_field,
};
use super::{
    auth_field, field, method, Event, Qos, SubscriptionStats, TopicInfo, CONNECT_ATTEMPTS,
    MAX_PAYLOAD, NAME,
};

/// A client of the topics broker over the bootstrap channel.
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    /// Resolve [`NAME`] and wrap the broker endpoint. Retries briefly
    /// while `messengerd` is still registering the name at boot.
    pub fn connect() -> Result<Client> {
        let first = match registry::resolve(NAME) {
            Ok(endpoint) => return Ok(Client { endpoint }),
            Err(error) => error,
        };
        if first.errno() != Some(-errno::ENOENT) {
            return Err(first);
        }
        // Park one tick per retry on a private pair; `close` frees the
        // pair when its last side goes (no channel is leaked). The probe
        // recv reuses one stack buffer because the bump heap never frees.
        let (probe, peer) = create_pair()?;
        let mut scratch = [0u8; 64];
        let mut client = Err(Error::Errno(-errno::ENOENT));
        for _ in 0..CONNECT_ATTEMPTS {
            let _ = probe.recv_into(&mut scratch, Some(EXPIRED_DEADLINE));
            match registry::resolve(NAME) {
                Ok(endpoint) => {
                    client = Ok(Client { endpoint });
                    break;
                }
                Err(error) if error.errno() == Some(-errno::ENOENT) => {}
                Err(error) => {
                    client = Err(error);
                    break;
                }
            }
        }
        let _ = probe.close();
        let _ = peer.close();
        client
    }

    /// Wrap an already-resolved broker endpoint.
    pub fn from_endpoint(endpoint: Endpoint) -> Client {
        Client { endpoint }
    }

    /// The underlying broker endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Run one request as a blocking call and fail on a broker error reply.
    fn call(&self, method: u32, body: Encoder, deadline: Option<u64>) -> Result<Parcel> {
        let reply = self
            .endpoint
            .call(&request_parcel(method, body), deadline)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Topics(code));
        }
        Ok(reply)
    }

    /// Publish an opaque payload parcel under `topic`; returns how many
    /// subscriptions matched.
    pub fn publish(&self, topic: &str, payload: &Parcel) -> Result<u64> {
        self.publish_inner(topic, payload, false)
    }

    /// Publish `payload` and remember it as the topic's retained value
    /// (`docs/messenger.md` section 7.2).
    pub fn publish_retained(&self, topic: &str, payload: &Parcel) -> Result<u64> {
        self.publish_inner(topic, payload, true)
    }

    fn publish_inner(&self, topic: &str, payload: &Parcel, retained: bool) -> Result<u64> {
        let bytes = super::super::endpoint::encode(payload)?;
        if bytes.len() > MAX_PAYLOAD {
            return Err(Error::Errno(-errno::E2BIG));
        }
        let reply = self.call(
            method::PUBLISH,
            publish_body(topic, &bytes, retained)?,
            None,
        )?;
        Ok(u64_field(&reply, field::MATCHED)?.unwrap_or(0))
    }

    /// Subscribe to `filter` (literal, `+` or trailing `#`) with `qos`.
    pub fn subscribe(&self, filter: &str, qos: Qos) -> Result<Subscription> {
        let reply = self.call(method::SUBSCRIBE, subscribe_body(filter, qos)?, None)?;
        let id = u64_field(&reply, field::SUBSCRIPTION)?.ok_or(Error::Errno(-errno::EINVAL))?;
        Ok(Subscription {
            endpoint: self.endpoint,
            id,
        })
    }

    /// Drop a subscription (same as [`Subscription::unsubscribe`]).
    pub fn unsubscribe(&self, subscription: &Subscription) -> Result<()> {
        self.call(
            method::UNSUBSCRIBE,
            subscription_body(subscription.id)?,
            None,
        )?;
        Ok(())
    }

    /// List topics the broker has seen, with live subscriber counts.
    pub fn list(&self) -> Result<Vec<TopicInfo>> {
        let reply = self.call(method::LIST_TOPICS, Encoder::new(), None)?;
        decode_topics(&reply)
    }

    /// Per-subscription counters (drops, queue depth, delivery).
    pub fn stats(&self, subscription: &Subscription) -> Result<SubscriptionStats> {
        let reply = self.call(method::STATS, subscription_body(subscription.id)?, None)?;
        decode_stats(&reply)
    }

    /// Round-trip probe.
    pub fn ping(&self) -> Result<()> {
        self.call(method::PING, Encoder::new(), None)?;
        Ok(())
    }
}

/// A live subscription on the broker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Subscription {
    endpoint: Endpoint,
    id: u64,
}

impl Subscription {
    /// The broker-side subscription id.
    pub const fn id(self) -> u64 {
        self.id
    }

    /// Wait for the next event; `Ok(None)` means the deadline passed.
    ///
    /// With `reliable` QoS the broker redelivers the head until it is
    /// acked, so the same event can come back more than once; call
    /// [`Subscription::ack`] once the payload is safely processed.
    pub fn next_event(&self, deadline: Option<u64>) -> Result<Option<Event>> {
        let reply = match self.endpoint.call(
            &request_parcel(method::NEXT_EVENT, subscription_body(self.id)?),
            deadline,
        ) {
            Ok(reply) => reply,
            // The kernel deadline is the timeout signal; the broker simply
            // discovers a dead pull when it later tries to answer it.
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => return Ok(None),
            Err(error) => return Err(error),
        };
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Topics(code));
        }
        decode_event(&reply)
    }

    /// [`Subscription::next_event`] with an already-expired deadline: never
    /// blocks, `Ok(None)` when nothing is queued.
    pub fn poll_event(&self) -> Result<Option<Event>> {
        self.next_event(Some(EXPIRED_DEADLINE))
    }

    /// Retire every event up to `sequence` (drives `reliable` queues).
    pub fn ack(&self, sequence: u64) -> Result<()> {
        let mut body = subscription_body(self.id)?;
        body.u64(field::SEQUENCE, sequence).map_err(Error::Parcel)?;
        let reply = self
            .endpoint
            .call(&request_parcel(method::ACK, body), None)?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Topics(code));
        }
        Ok(())
    }

    /// Per-subscription counters.
    pub fn stats(&self) -> Result<SubscriptionStats> {
        let reply = self.endpoint.call(
            &request_parcel(method::STATS, subscription_body(self.id)?),
            None,
        )?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Topics(code));
        }
        decode_stats(&reply)
    }

    /// Drop this subscription; later publishes stop matching it.
    pub fn unsubscribe(self) -> Result<()> {
        let reply = self.endpoint.call(
            &request_parcel(method::UNSUBSCRIBE, subscription_body(self.id)?),
            None,
        )?;
        if let Some(code) = error_field(&reply)? {
            return Err(Error::Topics(code));
        }
        Ok(())
    }
}

/// Ask the kernel policy engine about `name` for the task in `actor`
/// (the `messengerd` proxy path). `mode` is [`super::MODE_PUBLISH`] or
/// [`super::MODE_SUBSCRIBE`]; `txn` is copied into denial audit records.
///
/// `-EACCES` means policy refused a segment; the denial is already in the
/// audit ring.
pub fn authorize(actor: u64, mode: u32, name: &str, txn: u64) -> Result<()> {
    let mut body = Encoder::new();
    body.string(auth_field::NAME, name).map_err(Error::Parcel)?;
    body.u32(auth_field::MODE, mode).map_err(Error::Parcel)?;
    body.u64(auth_field::TXN, txn).map_err(Error::Parcel)?;
    // The kernel op ignores the parcel header; the body carries the query.
    let parcel = request_parcel(0, body);
    let bytes = super::super::endpoint::encode(&parcel)?;
    let args = MsgArgs {
        txn_id: actor,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    syscall(op::AUTHORIZE_TOPIC, &args, &mut result)?;
    Ok(())
}

/// Convenience: connect and publish. Reusing a [`Client`] is cheaper, but
/// this keeps one-shot callers short.
pub fn publish(topic: &str, payload: &Parcel) -> Result<u64> {
    Client::connect()?.publish(topic, payload)
}

/// Convenience: connect and publish a retained value.
pub fn publish_retained(topic: &str, payload: &Parcel) -> Result<u64> {
    Client::connect()?.publish_retained(topic, payload)
}

/// Convenience: connect and subscribe.
pub fn subscribe(filter: &str, qos: Qos) -> Result<Subscription> {
    Client::connect()?.subscribe(filter, qos)
}
