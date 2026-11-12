//! The Messenger methods: the registry, `init`'s services, the broker's
//! topics and one retained value. All read-only, through the same clients
//! `messengerctl` uses.

use alloc::format;
use alloc::string::String;

use dbgwire::json::{self, Object, Value};
use dbgwire::rpc::code;
use libmessenger::client::Deadline;
use user::messenger::client::Native;
use user::messenger::services::init::Client as InitClient;
use user::messenger::topics_client;

use super::handlers::Failure;

/// `msg.registry`: the kernel name registry.
pub(crate) fn msg_registry() -> Result<String, Failure> {
    // Through `messengerd`: the direct list call is for tasks that hold
    // `CAP_IPC_CONTROL`, the daemon lists for everyone.
    let entries = user::messenger::registry::Client::connect()
        .and_then(|client| client.list())
        .map_err(|e| (code::UNAVAILABLE, format!("registry: {}", e.message())))?;
    let rows = entries.iter().map(|e| {
        let interfaces = json::array(
            e.interfaces
                .iter()
                .map(|i| json::quoted(&format!("{i:#x}"))),
        );
        Object::new()
            .str("name", &e.name)
            .uint("object", e.object_id)
            .uint("owner_slot", e.owner_slot)
            .raw("interfaces", &interfaces)
            .uint("lease_ticks", e.lease_remaining)
            .finish()
    });
    Ok(Object::new().raw("services", &json::array(rows)).finish())
}

/// `msg.services`: what `init` supervises.
pub(crate) fn msg_services() -> Result<String, Failure> {
    let init = InitClient::try_connect(Native)
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    let statuses = init
        .services()
        .map_err(|e| (code::UNAVAILABLE, format!("init: {}", e.message())))?;
    let rows = statuses.iter().map(|s| {
        Object::new()
            .str("name", &s.name)
            .str("state", &s.state)
            .uint("pid", s.pid)
            .uint("restarts", s.restarts)
            .str("health", &s.health)
            .str("deps", &s.deps)
            .finish()
    });
    Ok(Object::new().raw("services", &json::array(rows)).finish())
}

/// `msg.topics`: the broker's topic list.
pub(crate) fn msg_topics() -> Result<String, Failure> {
    let client = topics_client::connect()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let entries = client
        .list_topics()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let rows = entries.iter().map(|t| {
        let payload = messenger_generated::declared_topic(&t.topic).map_or("-", |d| d.payload);
        Object::new()
            .str("topic", &t.topic)
            .uint("subscribers", t.subscribers)
            .bool("retained", t.retained)
            .str("payload", payload)
            .finish()
    });
    Ok(Object::new().raw("topics", &json::array(rows)).finish())
}

/// Longest payload returned in hex (bytes).
const MAX_TOPIC_PAYLOAD: usize = 1024;

/// `msg.topic`: the retained value an exact topic holds, if any. A
/// subscription replays it at once; none arriving means nothing is held.
pub(crate) fn msg_topic(params: &Value) -> Result<String, Failure> {
    let topic = params.get("topic").and_then(Value::as_str).ok_or_else(|| {
        (
            code::INVALID_PARAMS,
            String::from("msg.topic needs a topic"),
        )
    })?;
    if topic.is_empty() || topic.contains(['+', '#']) {
        return Err((
            code::INVALID_PARAMS,
            String::from("an exact topic name, no wildcards"),
        ));
    }
    let client = topics_client::connect()
        .map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let qos = topics_client::Qos::Latest;
    let id = client
        .subscribe(topic, qos.code(), qos.depth())
        .map_err(|e| (code::DENIED, format!("broker: {}", e.message())))?;
    // Raw, envelope and all: this shows what the broker holds.
    let polled = topics_client::next(&client, id, Deadline::Poll);
    let _ = client.unsubscribe(id);
    // `Ok(None)` is the answer "nothing is held"; an error is not.
    let event = polled.map_err(|e| (code::UNAVAILABLE, format!("broker: {}", e.message())))?;
    let Some(event) = event else {
        return Ok(Object::new()
            .str("topic", topic)
            .bool("held", false)
            .finish());
    };
    let shown = &event.payload[..event.payload.len().min(MAX_TOPIC_PAYLOAD)];
    Ok(Object::new()
        .str("topic", &event.topic)
        .bool("held", true)
        .bool("retained", event.retained)
        .uint("sequence", event.sequence)
        .uint("publisher_slot", event.publisher)
        .uint("payload_len", event.payload.len() as u64)
        .str("payload_hex", &dbgwire::auth::hex(shown))
        .finish())
}
