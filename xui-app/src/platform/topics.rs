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
//! at most what `idl/topics.midl` states for `Publish`, never a hang.

use libmessenger::envelope;
use messenger_generated::os_lazy_messenger_topics_v1 as wire;
use messenger_generated::topics;

use super::messenger::client;
use crate::sys::errno;

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

/// The central broker, resolved on the first publish (the client is kept for
/// the thread, and re-resolves when the broker restarts).
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
        let broker =
            client(wire::Client::try_connect).map_err(|_| PublishError::Errno(-errno::ENOENT))?;
        broker
            .publish(topic, &wrapped, retained)
            .map_err(|error| PublishError::Errno(error.code()))
    }
}
