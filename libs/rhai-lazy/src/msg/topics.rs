//! Topics from Rhai: `msg::publish`, `msg::subscribe` and the `Subscription`
//! value, on top of the broker's own interface (`os.lazy.messenger.topics.v1`,
//! served by `messengerd`). The broker is called through [`Service`], so this
//! module adds no wire code of its own.
//!
//! Payloads of a *declared* topic (`topic "..." : Type` in an IDL file) are
//! encoded from and decoded to Rhai values with the declared type; any other
//! topic carries raw bytes (a blob, or a string's UTF-8 bytes).
//!
//! On the broker every payload travels inside the platform's one-field
//! wrapper parcel ([`wrap`]/[`unwrap`]), exactly as the native services'
//! `central::Bus` (`user/src/central.rs`) writes and reads it, so a script
//! sees `confd`'s change events and a native subscriber sees a script's.

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};

use alloc::vec::Vec;

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};
use rhai::{Blob, Dynamic, ImmutableString, Map, INT};

use super::codec;
use super::schema::{self, Topic};
use super::service::{script_error, Fabric, Fallible, Service};

/// The broker's interface.
pub const BROKER: &str = "os.lazy.messenger.topics.v1";
/// The broker's `Qos` names, in wire order.
pub const QOS: [&str; 4] = ["latest", "buffered", "conflate", "reliable"];
/// Queue depth for `buffered` when the script does not choose one.
pub const DEFAULT_DEPTH: u32 = 16;

/// The wrapper parcel's interface marker (`central::WRAPPER_INTERFACE`).
const WRAPPER_INTERFACE: u64 = u64::from_le_bytes(*b"os.cntrl");
/// Wrapper field holding a UTF-8 payload / any other payload.
const FIELD_TEXT: u16 = 1;
const FIELD_BYTES: u16 = 2;

/// Wrap payload bytes the way `central::Bus::publish` does: text in field 1,
/// anything else in field 2, inside a parcel stamped [`WRAPPER_INTERFACE`].
pub fn wrap(payload: &[u8]) -> Fallible<Vec<u8>> {
    let mut body = Encoder::new();
    let written = match core::str::from_utf8(payload) {
        Ok(text) => body.string(FIELD_TEXT, text),
        Err(_) => body.bytes(FIELD_BYTES, payload),
    };
    let parcel = Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: WRAPPER_INTERFACE,
            method: 1,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: written
            .map(|()| body.finish())
            .map_err(|e| script_error(e.message()))?,
        handles: Vec::new(),
        buffers: Vec::new(),
    };
    let mut bytes = Vec::new();
    parcel
        .encode(&mut bytes)
        .map_err(|e| script_error(e.message()))?;
    Ok(bytes)
}

/// The payload inside a wrapper parcel; anything else (another publisher's
/// own parcel, raw bytes) is handed through unchanged, like `central`'s
/// `unwrap_event`.
pub fn unwrap(event_payload: &[u8]) -> Vec<u8> {
    let Ok(parcel) = Parcel::decode(event_payload) else {
        return event_payload.to_vec();
    };
    if parcel.header.interface_id != WRAPPER_INTERFACE {
        return event_payload.to_vec();
    }
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        match (field.kind, field.id) {
            (Kind::String, FIELD_TEXT) | (Kind::Bytes, FIELD_BYTES) => {
                return field.as_bytes().to_vec()
            }
            _ => {}
        }
    }
    event_payload.to_vec()
}

fn broker(fabric: &Rc<Fabric>) -> Fallible<Service> {
    Service::connect(fabric.clone(), BROKER, None)
}

/// The declared topic a concrete name or filter falls under, if any.
fn declared(topic: &str) -> Option<(&'static schema::Interface, &'static Topic)> {
    schema::declared_topic(topic)
}

/// A live subscription. Cheap to clone; every clone pulls the same queue.
#[derive(Clone)]
pub struct Subscription {
    pub(crate) id: INT,
    pub(crate) filter: String,
    pub(crate) qos: u32,
    pub(crate) broker: Service,
}

impl core::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Subscription({} #{} {})",
            self.filter,
            self.id,
            QOS[self.qos as usize % 4]
        )
    }
}

/// `#{ qos: "reliable", depth: 32 }` -> (qos index, depth); missing keys take
/// the declared topic's QoS (else `buffered`) and [`DEFAULT_DEPTH`].
fn options(filter: &str, opts: &Map) -> Fallible<(u32, u32)> {
    if let Some(key) = opts.keys().find(|k| *k != "qos" && *k != "depth") {
        return Err(script_error(format!(
            "msg::subscribe: unknown option `{key}` (qos, depth)"
        )));
    }
    let qos = match opts.get("qos") {
        Some(name) => {
            let name = name.to_string();
            QOS.iter().position(|q| *q == name).ok_or_else(|| {
                script_error(format!(
                    "msg::subscribe: qos `{name}` is not one of {}",
                    QOS.join(", ")
                ))
            })? as u32
        }
        None => declared(filter).map_or(1, |(_, t)| t.qos),
    };
    let depth = match opts.get("depth") {
        Some(depth) => {
            let n = depth
                .as_int()
                .map_err(|_| script_error("msg::subscribe: depth must be an integer"))?;
            u32::try_from(n)
                .ok()
                .filter(|d| (1..=64).contains(d))
                .ok_or_else(|| script_error("msg::subscribe: depth must be 1..=64"))?
        }
        None => DEFAULT_DEPTH,
    };
    Ok((qos, depth))
}

/// `msg::subscribe(filter[, options])`.
pub fn subscribe(fabric: &Rc<Fabric>, filter: &str, opts: &Map) -> Fallible<Subscription> {
    let (qos, depth) = options(filter, opts)?;
    let broker = broker(fabric)?;
    let args = Dynamic::from_array(alloc::vec![
        Dynamic::from(String::from(filter)),
        Dynamic::from_int(qos.into()),
        Dynamic::from_int(depth.into()),
    ]);
    let id = broker
        .invoke("Subscribe", args)?
        .as_int()
        .map_err(script_error)?;
    Ok(Subscription {
        id,
        filter: filter.into(),
        qos,
        broker,
    })
}

/// The bytes a topic carries for `value`.
fn payload_bytes(topic: &str, value: &Dynamic) -> Fallible<Blob> {
    if let Some((iface, decl)) = declared(topic) {
        return codec::encode_payload(iface, decl.payload, value)
            .map_err(|e| script_error(format!("msg::publish: {topic}: {e}")));
    }
    if let Some(blob) = value.read_lock::<Blob>() {
        return Ok(blob.clone());
    }
    if let Some(text) = value.read_lock::<ImmutableString>() {
        return Ok(text.as_bytes().into());
    }
    Err(script_error(format!(
        "msg::publish: {topic} is not a declared topic, so its payload must be a blob or string (got {})",
        value.type_name()
    )))
}

/// `msg::publish(topic, value[, retained])`: the number of subscriptions the
/// event reached. `retained` defaults to the declaration (else `false`).
pub fn publish(
    fabric: &Rc<Fabric>,
    topic: &str,
    value: &Dynamic,
    retained: Option<bool>,
) -> Fallible<INT> {
    let bytes = payload_bytes(topic, value)?;
    let retained = retained.unwrap_or_else(|| declared(topic).is_some_and(|(_, t)| t.retained));
    let args = Dynamic::from_array(alloc::vec![
        Dynamic::from(String::from(topic)),
        Dynamic::from_blob(wrap(&bytes)?),
        Dynamic::from_bool(retained),
    ]);
    broker(fabric)?
        .invoke("Publish", args)?
        .as_int()
        .map_err(script_error)
}

/// The broker's `Event` as a script sees it: the metadata, the decoded
/// `payload` (raw bytes for an undeclared topic) and the raw `bytes`.
fn event_map(event: Dynamic) -> Fallible<Dynamic> {
    let mut map = event
        .try_cast::<Map>()
        .ok_or_else(|| script_error("msg: the broker sent a malformed event"))?;
    let topic = map.get("topic").map(|t| t.to_string()).unwrap_or_default();
    let bytes = unwrap(
        &map.remove("payload")
            .and_then(|p| p.try_cast::<Blob>())
            .unwrap_or_default(),
    );
    let payload = match declared(&topic) {
        Some((iface, decl)) => codec::decode_payload(iface, decl.payload, &bytes)
            .map_err(|e| script_error(format!("msg: event on {topic}: {e}")))?,
        None => Dynamic::from_blob(bytes.clone()),
    };
    map.insert("payload".into(), payload);
    map.insert("bytes".into(), Dynamic::from_blob(bytes));
    Ok(Dynamic::from_map(map))
}

impl Subscription {
    /// The next event, waiting at most `timeout_ms` (`0` = forever); `()`
    /// when none arrived in time.
    pub fn next(&self, timeout_ms: u64) -> Fallible<Dynamic> {
        match self
            .broker
            .invoke_within("NextEvent", Dynamic::from_int(self.id), timeout_ms)?
        {
            Some(event) => event_map(event),
            None => Ok(Dynamic::UNIT),
        }
    }

    /// Retire every event up to `sequence` (needed for `reliable`).
    pub fn ack(&self, sequence: INT) -> Fallible<()> {
        let args = Dynamic::from_array(alloc::vec![
            Dynamic::from_int(self.id),
            Dynamic::from_int(sequence)
        ]);
        self.broker.invoke("Ack", args).map(|_| ())
    }

    /// Drop the subscription; later publishes stop matching it.
    pub fn close(&self) -> Fallible<()> {
        self.broker
            .invoke("Unsubscribe", Dynamic::from_int(self.id))
            .map(|_| ())
    }

    pub fn is_reliable(&self) -> bool {
        self.qos == 3
    }
}
