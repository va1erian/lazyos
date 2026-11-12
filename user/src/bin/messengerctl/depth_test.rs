//! A hostile `Buffered` depth (audit #5): the Messenger client clamps the depth
//! it asks for, so this test calls the generated broker client with
//! `depth = u32::MAX`, as a malicious client would, and checks the broker still
//! queues at most `Qos::MAX_DEPTH` events.

use alloc::format;
use alloc::string::String;
use messenger_generated::os_lazy_messenger_topics_v1 as generated;
use user::central::Bus;
use user::messenger::topics_client;

use super::topic_tests::{err_text, test_payload};

const TOPIC: &str = generated::TOPIC_TOPICS_DEPTH;

/// Subscribe with an unclamped depth, publish past `MAX_DEPTH`, and read the
/// subscription's counters: it holds `MAX_DEPTH` events and dropped the rest.
pub(crate) fn selftest_depth(bus: &mut Bus) -> Result<(), String> {
    let refused = |error: topics_client::ClientError| String::from(error.message());
    let id = bus
        .client()
        .subscribe(TOPIC, generated::QOS_BUFFERED, u32::MAX)
        .map_err(refused)?;
    let max = u64::from(topics_client::Qos::MAX_DEPTH);
    let extra = 8;
    let payload = test_payload("depth")?;
    for _ in 0..max + extra {
        bus.publish(TOPIC, &payload, false).map_err(err_text)?;
    }
    let stats = bus.client().stats(id).map_err(refused)?;
    bus.client().unsubscribe(id).map_err(refused)?;
    if stats.queued != max || stats.drops != extra {
        return Err(format!(
            "depth {} queued {} drops {}, expected {max} queued and {extra} drops",
            stats.depth, stats.queued, stats.drops
        ));
    }
    Ok(())
}
