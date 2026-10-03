//! The service calls the install handoff makes, over a [`Transport`], with the
//! `midlc`-generated stubs only (`idl/mimed.midl`, `idl/init.midl`,
//! `idl/topics.midl`, `idl/pkgd.midl`): no field id or method number is written
//! here.

use messenger_generated::os_lazy_init_v1 as init_wire;
use messenger_generated::os_lazy_messenger_topics_v1 as topics;
use messenger_generated::os_lazy_mimed_v1 as mimed;
use messenger_generated::os_lazy_pkgd_v1 as pkgd;

use rhai_lazy::msg::topics as topics_wrapper;

use crate::transport::{Failure, Transport, Wait};

/// The registered service names (not the `.vN` interface names).
pub const MIMED_NAME: &str = "os.lazy.mimed";
/// `init`'s service name.
pub const INIT_NAME: &str = "os.lazy.init";
/// The topics broker's service name.
pub const BROKER_NAME: &str = "os.lazy.messenger.topics";

/// Events a package subscription may queue before the broker drops the oldest:
/// an install is one event, but another install or the provisioning pass may
/// publish while the user reads the consent screen.
const QUEUE_DEPTH: u32 = 64;

/// The calls the IDE makes, over a [`Transport`].
pub struct Client<T: Transport> {
    pub(super) transport: T,
}

impl<T: Transport> Client<T> {
    /// A client over `transport`.
    pub fn new(transport: T) -> Client<T> {
        Client { transport }
    }

    /// `mimed.Open(path, verb)`: `mimed` resolves the app registered for the
    /// verb and asks `init` to launch it in this caller's session.
    pub fn open(&self, path: &str, verb: &str) -> Result<mimed::OpenReply, Failure> {
        let body = mimed::encode_open_args(&mimed::OpenArgs {
            path: path.to_owned(),
            verb: verb.to_owned(),
        })
        .map_err(|_| bad_request())?;
        let reply = self.transport.call(
            MIMED_NAME,
            mimed::INTERFACE_ID,
            mimed::METHOD_OPEN,
            body,
            Wait::Forever,
        )?;
        mimed::decode_open_reply(&reply).map_err(|_| garbled("the file-type service"))
    }

    /// `init.Launch(system_name)` in the caller's own session.
    pub fn launch(&self, system_name: &str) -> Result<(), Failure> {
        let body = init_wire::encode_launch_args(&init_wire::LaunchArgs {
            app: system_name.to_owned(),
            args: String::new(),
            session: 0,
        })
        .map_err(|_| bad_request())?;
        self.transport
            .call(
                INIT_NAME,
                init_wire::INTERFACE_ID,
                init_wire::METHOD_LAUNCH,
                body,
                Wait::Forever,
            )
            .map(|_| ())
    }

    /// Subscribes to `system/events/pkg/+`, the package manager's audit
    /// events, and returns the subscription id.
    pub fn subscribe_pkg_events(&self) -> Result<u64, Failure> {
        let body = topics::encode_subscribe_args(&topics::SubscribeArgs {
            filter: pkgd::TOPIC_SYSTEM_EVENTS_PKG.to_owned(),
            qos: topics::QOS_BUFFERED,
            depth: QUEUE_DEPTH,
        })
        .map_err(|_| bad_request())?;
        let reply = self.broker(topics::METHOD_SUBSCRIBE, body, Wait::Forever)?;
        topics::decode_subscribe_reply(&reply)
            .map(|reply| reply.subscription)
            .map_err(|_| garbled("the topics broker"))
    }

    /// Drops a subscription (best effort: it is released with the task anyway).
    pub fn unsubscribe(&self, subscription: u64) {
        if let Ok(body) = topics::encode_unsubscribe_args(&topics::UnsubscribeArgs { subscription })
        {
            let _ = self.broker(topics::METHOD_UNSUBSCRIBE, body, Wait::Forever);
        }
    }

    /// The next package event of `subscription`, `None` when none is queued.
    /// An event whose payload cannot be read comes back empty, so it matches
    /// nothing instead of ending the wait.
    pub fn next_pkg_event(&self, subscription: u64) -> Result<Option<pkgd::PkgEvent>, Failure> {
        let body = topics::encode_next_event_args(&topics::NextEventArgs { subscription })
            .map_err(|_| bad_request())?;
        let reply = match self.broker(topics::METHOD_NEXTEVENT, body, Wait::Poll) {
            Ok(reply) => reply,
            Err(failure) if failure.is_timeout() => return Ok(None),
            Err(failure) => return Err(failure),
        };
        let event = topics::decode_next_event_reply(&reply)
            .map_err(|_| garbled("the topics broker"))?
            .event;
        if event.topic.is_empty() {
            return Ok(None);
        }
        // The broker carries the publisher's payload in `central`'s wrapper
        // parcel (`Bus::publish`); `unwrap` is the one reader of that envelope.
        let payload = topics_wrapper::unwrap(&event.payload);
        Ok(Some(pkgd::decode_pkg_event(&payload).unwrap_or_default()))
    }

    fn broker(&self, method: u32, body: Vec<u8>, wait: Wait) -> Result<Vec<u8>, Failure> {
        self.transport
            .call(BROKER_NAME, topics::INTERFACE_ID, method, body, wait)
    }
}

fn bad_request() -> Failure {
    Failure {
        code: 22,
        text: "the request could not be encoded".to_owned(),
    }
}

fn garbled(who: &str) -> Failure {
    Failure {
        code: -22,
        text: format!("{who} sent a reply this program cannot read"),
    }
}
