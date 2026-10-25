//! A hostile `Buffered` depth (audit #5): the client library clamps the depth
//! it encodes, so this test hand-encodes `Subscribe` with `depth = u32::MAX`,
//! as a malicious client would, and checks the broker still queues at most
//! `Qos::MAX_DEPTH` events.

use alloc::format;
use alloc::string::String;
use libmessenger::Parcel;
use messenger_generated::os_lazy_messenger_topics_v1 as generated;
use user::messenger::{self, topics_client};

use super::topic_tests::test_parcel;

const TOPIC: &str = "topics/depth";

fn err_text(error: messenger::Error) -> String {
    String::from(error.message())
}

/// One raw broker call that fails on an error reply, like the client's own.
fn call(client: &topics_client::Client, request: Parcel) -> Result<Parcel, String> {
    let reply = client.endpoint().call(&request, None).map_err(err_text)?;
    match topics_client::error_field(&reply).map_err(err_text)? {
        Some(code) => Err(format!("broker error {code}")),
        None => Ok(reply),
    }
}

/// Subscribe with an unclamped depth, publish past `MAX_DEPTH`, and read the
/// subscription's counters: it holds `MAX_DEPTH` events and dropped the rest.
pub(crate) fn selftest_depth(client: &topics_client::Client) -> Result<(), String> {
    let args = generated::SubscribeArgs {
        filter: String::from(TOPIC),
        qos: generated::QOS_BUFFERED,
        depth: u32::MAX,
    };
    let body = generated::encode_subscribe_args(&args).map_err(|e| String::from(e.message()))?;
    let reply = call(
        client,
        topics_client::request_parcel(topics_client::method::SUBSCRIBE, body),
    )?;
    let id = topics_client::decode_subscription(&reply).map_err(err_text)?;
    let max = u64::from(topics_client::Qos::MAX_DEPTH);
    let extra = 8;
    let payload = test_parcel("depth")?;
    for _ in 0..max + extra {
        client.publish(TOPIC, &payload).map_err(err_text)?;
    }
    let stats = call(
        client,
        topics_client::subscription_request(topics_client::method::STATS, id).map_err(err_text)?,
    )?;
    let stats = topics_client::decode_stats(&stats).map_err(err_text)?;
    call(
        client,
        topics_client::subscription_request(topics_client::method::UNSUBSCRIBE, id)
            .map_err(err_text)?,
    )?;
    if stats.queued != max || stats.drops != extra {
        return Err(format!(
            "depth {} queued {} drops {}, expected {max} queued and {extra} drops",
            stats.depth, stats.queued, stats.drops
        ));
    }
    Ok(())
}
