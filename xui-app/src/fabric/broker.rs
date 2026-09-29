//! The userspace topics broker (`messengerd`'s `os.lazy.messenger.topics`
//! service): its `list_topics` call and the entry records it returns.

use libmessenger::{flags, Decoder, Encoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, msg_op, MsgArgs, MsgResult};

use super::error::{error_code, E2BIG, EINVAL, ENOENT, EPIPE};
use super::registry::{close, resolve};

/// The topics-broker well-known name.
pub const TOPICS_NAME: &str = "os.lazy.messenger.topics";
/// Topics interface id: `fnv1a64("os.lazy.messenger.topics.v1")`.
const TOPICS_INTERFACE: u64 = 0xc573_4f97_8fef_7231;
/// Broker method `list_topics` (`fnv1a32` of the method name).
const TOPICS_LIST_TOPICS: u32 = 225_427_937;
/// Broker TLV field ids used here.
mod topics_field {
    pub const TOPIC: u16 = 1;
    pub const SUBSCRIBERS: u16 = 15;
    pub const ENTRY: u16 = 16;
}

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
                interface_id: TOPICS_INTERFACE,
                method: TOPICS_LIST_TOPICS,
                txn_id: 0,
                reply_to: 0,
                deadline_ns: 0,
            },
            body: Encoder::new().finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
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
    let mut bytes = Vec::new();
    request.encode(&mut bytes).map_err(|_| -EINVAL)?;
    let args = MsgArgs {
        handle,
        parcel_ptr: bytes.as_ptr() as u64,
        parcel_len: bytes.len() as u64,
        buf_ptr: buf.as_mut_ptr() as u64,
        buf_cap: buf.len() as u64,
        deadline: sys::clock_ticks().saturating_add(BROKER_CALL_TICKS),
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::CALL,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        return Err(code);
    }
    let len = result.bytes as usize;
    if len > buf.len() {
        return Err(-E2BIG);
    }
    Ok(len)
}

/// Decode the broker's `ENTRY` topic records.
fn decode_topics(parcel: &Parcel) -> Option<Vec<TopicRow>> {
    let mut topics = Vec::new();
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(record)) = decoder.next() {
        if record.kind != Kind::Struct || record.id != topics_field::ENTRY {
            continue;
        }
        let mut nested = record.nested(0).ok()?;
        let mut row = TopicRow {
            topic: String::new(),
            subscribers: 0,
            retained: false,
        };
        while let Ok(Some(item)) = nested.next() {
            match (item.kind, item.id) {
                (Kind::String, topics_field::TOPIC) => row.topic = item.as_str().ok()?.to_string(),
                (Kind::U64, topics_field::SUBSCRIBERS) => row.subscribers = item.as_u64().ok()?,
                (Kind::Bool, _) => row.retained = item.as_bool().ok()?,
                _ => {}
            }
        }
        topics.push(row);
    }
    Some(topics)
}
