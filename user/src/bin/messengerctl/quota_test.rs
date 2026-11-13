//! The broker's byte budget at run time (`TOPIC:QUOTA`): readers that never
//! read must not make a publish fail, must stay inside one task's allowance,
//! and must give every byte back when they unsubscribe. This task is a
//! platform one (no label, no session), so the system allowance applies; the
//! app allowance and the reserve are covered by the host tests of
//! `libs/topicsbroker`.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use messenger_generated::os_lazy_messenger_topics_v1 as generated;
use user::central::Bus;
use user::messenger::topics_client;

const TOPIC: &str = "selftest/quota/flood";
/// Subscriptions that never read (a platform task may hold
/// `topicsbroker::SYSTEM_TENANT_SUBSCRIPTIONS`, 64).
const SLOW_READERS: usize = 40;
/// Publishes of a maximum payload: far more than the allowance can queue.
const PUBLISHES: usize = 200;
/// One platform task's allowance (`topicsbroker::SYSTEM_TENANT_BYTES`).
const ALLOWANCE: u64 = topicsbroker::SYSTEM_TENANT_BYTES as u64;

fn refused(error: topics_client::ClientError) -> String {
    String::from(error.message())
}

/// Flood slow readers with full-size events: every publish succeeds, what the
/// readers hold stays inside the allowance (the rest are counted drops), and
/// after they unsubscribe a fresh queue fills to its full depth again.
pub(crate) fn selftest_quota(bus: &mut Bus) -> Result<(), String> {
    let mut ids: Vec<u64> = Vec::new();
    for _ in 0..SLOW_READERS {
        let id = bus
            .client()
            .subscribe(TOPIC, generated::QOS_BUFFERED, u32::MAX)
            .map_err(refused)?;
        ids.push(id);
    }
    let payload = alloc::vec![0x5a_u8; topics_client::MAX_PAYLOAD];
    for round in 0..PUBLISHES {
        bus.client()
            .publish(TOPIC, &payload, false)
            .map_err(|error| format!("publish {round} refused: {}", error.message()))?;
    }
    let (mut queued, mut drops) = (0u64, 0u64);
    for id in &ids {
        let stats = bus.client().stats(*id).map_err(refused)?;
        queued += stats.queued;
        drops += stats.drops;
    }
    for id in ids {
        bus.client().unsubscribe(id).map_err(refused)?;
    }
    // What the broker charges per event: payload, topic and its flat record.
    let held = queued * (payload.len() + TOPIC.len() + topicsbroker::EVENT_OVERHEAD) as u64;
    if held > ALLOWANCE || drops == 0 {
        return Err(format!(
            "slow readers held {held} bytes ({queued} events, {drops} drops) of {ALLOWANCE}"
        ));
    }
    refill(bus, &payload)
}

/// The released bytes are usable again: one subscription takes a full
/// `MAX_DEPTH` of full-size events.
fn refill(bus: &mut Bus, payload: &[u8]) -> Result<(), String> {
    let id = bus
        .client()
        .subscribe(TOPIC, generated::QOS_BUFFERED, u32::MAX)
        .map_err(refused)?;
    let depth = u64::from(topics_client::Qos::MAX_DEPTH);
    for _ in 0..depth {
        bus.client()
            .publish(TOPIC, payload, false)
            .map_err(refused)?;
    }
    let stats = bus.client().stats(id).map_err(refused)?;
    bus.client().unsubscribe(id).map_err(refused)?;
    if stats.queued != depth || stats.drops != 0 {
        return Err(format!(
            "after release a queue held {} of {depth} ({} drops)",
            stats.queued, stats.drops
        ));
    }
    Ok(())
}
