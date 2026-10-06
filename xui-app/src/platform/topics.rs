//! Publishing on the central topics broker (`os.lazy.messenger.topics`,
//! served by `messengerd`) from an xui app.
//!
//! [`Broker`] implements the generated [`topics::Publish`] seam, so an app
//! publishes a declared topic through its `midlc` helper
//! (`publish_session_selection(&mut Broker::new(), ...)`) and never spells a
//! topic name or a payload layout by hand. Payloads go inside the platform's
//! one envelope (`libmessenger::envelope`), exactly as the native
//! `user::central::Bus` and the Rhai `msg` module write them, so every
//! subscriber reads them the same way.
//!
//! Each call is bounded: a broker that stopped answering costs the UI thread
//! at most [`PUBLISH_TICKS`], never a hang.

use libmessenger::envelope;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;
use messenger_generated::topics;

use super::messenger::Service;
use crate::server::ERROR_FIELD;

/// The broker's registered name.
const BROKER: &str = "os.lazy.messenger.topics";
/// Ticks (100 Hz) one publish may take.
pub const PUBLISH_TICKS: u64 = 50;

/// Why a publish failed: a negative errno from the broker or the kernel, or a
/// topic the generated helper refused to build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublishError {
    /// A negative errno (`-ENOENT` when no broker is registered).
    Errno(i64),
    /// The topic name or payload could not be built.
    Topic(topics::TopicError),
}

impl From<topics::TopicError> for PublishError {
    fn from(error: topics::TopicError) -> Self {
        PublishError::Topic(error)
    }
}

/// The central broker, resolved on each publish (the endpoint is cached by
/// [`Service`], and evicted when the broker restarts).
#[derive(Default)]
pub struct Broker;

impl Broker {
    pub const fn new() -> Broker {
        Broker
    }
}

impl topics::Publish for Broker {
    type Error = PublishError;

    fn publish_topic(
        &mut self,
        topic: &str,
        payload: &[u8],
        retained: bool,
    ) -> Result<u64, PublishError> {
        let wrapped = envelope::wrap(payload)
            .map_err(|error| PublishError::Topic(topics::TopicError::Encode(error)))?;
        let body = wire::encode_publish_args(&wire::PublishArgs {
            topic: topic.to_owned(),
            payload: wrapped,
            retained,
        })
        .map_err(|error| PublishError::Topic(topics::TopicError::Encode(error)))?;
        let broker = Service::try_connect(BROKER).ok_or(PublishError::Errno(-2))?;
        let reply = broker
            .call_within(
                wire::INTERFACE_ID,
                wire::METHOD_PUBLISH,
                ERROR_FIELD,
                body,
                PUBLISH_TICKS,
            )
            .map_err(PublishError::Errno)?;
        wire::decode_publish_reply(&reply.body)
            .map(|reply| reply.matched)
            .map_err(|error| PublishError::Topic(topics::TopicError::Encode(error)))
    }
}
