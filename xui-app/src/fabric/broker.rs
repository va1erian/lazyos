//! The userspace topics broker (`messengerd`'s `os.lazy.messenger.topics`
//! service): its `list_topics` call and the entry records it returns.

use libmessenger::{flags, Header, Parcel, VERSION};
use messenger_generated::os_lazy_messenger_topics_v1 as wire;

use crate::sys;

use super::error::{error_code, EINVAL, ENOENT, EPIPE};
use super::registry::{close, resolve};

/// The topics-broker well-known name.
pub const TOPICS_NAME: &str = "os.lazy.messenger.topics";
/// Ticks a broker call waits before it gives up (`2 s` at 100 Hz), so a stalled
/// broker cannot freeze the dashboard forever.
const BROKER_CALL_TICKS: u64 = 200;

/// One topic the broker has seen, with its live subscriber count.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TopicRow {
    /// Topic name.
    pub topic: String,
    /// Live subscriptions whose filter matches it.
    pub subscribers: u64,
    /// Whether the broker holds a retained value for it.
    pub retained: bool,
}

/// Where the topics panel's data came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Topics {
    /// The broker answered with its topic list.
    Online(Vec<TopicRow>),
    /// No broker is running (or it refused/failed); the negative errno.
    Offline(i64),
}

/// A cached topics-broker endpoint.
///
/// `CLOSE_ENDPOINT` marks an endpoint closed for every holder — including the
/// kernel's published service side — so a client that closes its resolved
/// handle after a call kills the channel the broker receives on. The monitor
/// therefore resolves the broker once and reuses the handle for every refresh;
/// only the kernel's own registry `resolve`/`list` paths are one-shot.
pub struct Broker {
    endpoint: Option<u64>,
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    /// No endpoint resolved yet.
    pub const fn new() -> Broker {
        Broker { endpoint: None }
    }

    /// List topics through the userspace broker, resolving it on first use.
    pub fn topics(&mut self) -> Topics {
        let request = Parcel {
            header: Header {
                version: VERSION,
                flags: flags::SYNC | flags::ALLOW_NESTED,
                interface_id: wire::INTERFACE_ID,
                method: wire::METHOD_LISTTOPICS,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: Vec::new(),
            objects: Vec::new(),
        };
        let mut buf = vec![0u8; 16 * 1024];
        let len = match self.call(&request, &mut buf) {
            Ok(len) => len,
            Err(code) => return Topics::Offline(code),
        };
        let parcel = match Parcel::decode(&buf[..len]) {
            Ok(parcel) => parcel,
            Err(_) => return Topics::Offline(-EINVAL),
        };
        if let Some(error) = error_code(&parcel) {
            return Topics::Offline(error);
        }
        match decode_topics(&parcel) {
            Some(topics) => Topics::Online(topics),
            None => Topics::Offline(-EINVAL),
        }
    }

    /// Run one request on the cached endpoint, resolving it first if needed.
    fn call(&mut self, request: &Parcel, buf: &mut [u8]) -> Result<usize, i64> {
        let endpoint = match self.endpoint {
            Some(endpoint) => endpoint,
            None => {
                let endpoint = resolve(TOPICS_NAME)?;
                self.endpoint = Some(endpoint);
                endpoint
            }
        };
        match call_on(endpoint, request, buf) {
            Ok(len) => Ok(len),
            Err(code) => {
                // A handle that names a dead endpoint (or no longer exists)
                // cannot recover; release the table slot and resolve fresh on
                // the next refresh. Transient failures (a timeout) keep it.
                if code == -ENOENT || code == -EPIPE {
                    self.endpoint = None;
                    let _ = close(endpoint);
                }
                Err(code)
            }
        }
    }
}

/// A blocking call with the broker's deadline; returns the reply length.
fn call_on(handle: u64, request: &Parcel, buf: &mut [u8]) -> Result<usize, i64> {
    let bytes = lazyos_sys::msg::parcel::encode(request)?;
    let deadline = sys::clock_ticks().saturating_add(BROKER_CALL_TICKS);
    lazyos_sys::msg::call(handle, &bytes, buf, deadline)
}

/// Decode the generated `ListTopics` reply into display rows.
fn decode_topics(parcel: &Parcel) -> Option<Vec<TopicRow>> {
    let reply = wire::decode_list_topics_reply(&parcel.body).ok()?;
    Some(
        reply
            .topics
            .into_iter()
            .map(|info| TopicRow {
                topic: info.topic,
                subscribers: info.subscribers,
                retained: info.retained,
            })
            .collect(),
    )
}
