//! The client half of the interim topic router: connect to a service's
//! broker, subscribe, publish (split out of `router.rs`).

use super::*;

/// The client half: connect to a service's broker, subscribe, publish.
pub struct Bus {
    endpoint: Endpoint,
}

impl Bus {
    /// Resolve `name` (a broker service, e.g. `os.lazy.healthd`).
    pub fn connect(name: &str) -> Result<Bus> {
        Ok(Bus {
            endpoint: registry::resolve(name)?,
        })
    }

    /// The underlying broker endpoint (diagnostics).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Subscribe to `filter`; returns a receiver already attached to the
    /// broker. Retained matching values are queued by the broker.
    pub fn subscribe(&self, filter: &str) -> Result<Subscriber> {
        // One round trip reserves a unique sink name; the subscriber then
        // registers its own endpoint under it, so no handle transfer is
        // needed across processes.
        let reply = self
            .endpoint
            .call(&parcel(method::RESERVE, Encoder::new()), None)?;
        let sink = string_field(&reply, field::SINK)?;
        let (published, received) = create_pair()?;
        if let Err(error) = registry::register(&sink, &published, &[], 0) {
            let _ = published.close();
            let _ = received.close();
            return Err(error);
        }

        let mut body = Encoder::new();
        let encoded = body
            .string(field::SINK, &sink)
            .and_then(|()| body.string(field::FILTER, filter))
            .map_err(Error::Parcel);
        if let Err(error) = encoded {
            let _ = registry::unregister(&sink);
            let _ = published.close();
            let _ = received.close();
            return Err(error);
        }
        if let Err(error) = self.endpoint.call(&parcel(method::SUBSCRIBE, body), None) {
            // The broker refused (full, or gone): the name and both ends would
            // outlive the attempt, and a caller that retries (`logins`, every
            // ten seconds) would take a registry name each time (audit M-R4).
            let _ = registry::unregister(&sink);
            let _ = published.close();
            let _ = received.close();
            return Err(error);
        }
        Ok(Subscriber {
            endpoint: received,
            filter: String::from(filter),
        })
    }

    /// Publish `payload` on `topic` through the broker.
    pub fn publish(&self, topic: &str, payload: &[u8], retained: bool) -> Result<()> {
        let mut body = Encoder::new();
        body.string(field::TOPIC, topic).map_err(Error::Parcel)?;
        body.bytes(field::PAYLOAD, payload).map_err(Error::Parcel)?;
        body.u64(field::RETAINED, retained as u64)
            .map_err(Error::Parcel)?;
        self.endpoint.call(&parcel(method::PUBLISH, body), None)?;
        Ok(())
    }
}

/// The transport the generated `topic` helpers publish through when the
/// publisher is a client of someone else's broker (e.g. `logind` publishing on
/// `init`'s). The interim `Bus::publish` discards the broker's fanout count.
impl messenger_generated::topics::Publish for Bus {
    type Error = Error;

    fn publish_topic(&mut self, topic: &str, payload: &[u8], retained: bool) -> Result<u64> {
        self.publish(topic, payload, retained).map(|()| 0)
    }
}

/// The transport the generated `topic` helpers subscribe through. The interim
/// router has no QoS queues, so the declared `qos` is accepted and ignored.
impl messenger_generated::topics::Subscribe for Bus {
    type Error = Error;
    type Subscription = Subscriber;

    fn subscribe_topic(&mut self, filter: &str, _qos: u32) -> Result<Subscriber> {
        self.subscribe(filter)
    }
}

/// A subscriber's receiving end.
pub struct Subscriber {
    endpoint: Endpoint,
    /// The filter this subscriber attached with.
    pub filter: String,
}

impl Subscriber {
    /// The endpoint the broker pushes events into, to park on beside others
    /// (`wait::wait_any`).
    pub fn endpoint(&self) -> Endpoint {
        self.endpoint
    }

    /// Receive the next event, or `None` when `deadline` passes first.
    ///
    /// Allocates the receive buffer per call; a polling loop should use
    /// [`Subscriber::recv_with`] and reuse one buffer.
    pub fn recv(&self, deadline: Option<u64>) -> Result<Option<Event>> {
        let mut buf = alloc::vec![0u8; crate::messenger::DEFAULT_BUFFER];
        self.recv_with(&mut buf, deadline)
    }

    /// [`Subscriber::recv`] with a caller-owned buffer.
    pub fn recv_with(&self, buf: &mut [u8], deadline: Option<u64>) -> Result<Option<Event>> {
        match self.endpoint.recv_with(buf, deadline) {
            Ok(message) => Ok(Some(Event::from_message(&message)?)),
            Err(Error::Errno(code)) if code == -errno::ETIMEDOUT => Ok(None),
            Err(error) => Err(error),
        }
    }
}
