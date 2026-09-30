//! `nicctl soak=<n>`: attach, exchange one ARP request with the gateway,
//! detach, `n` times in a row.
//!
//! A leak of anything per attachment (a handle, a mapping, a shared buffer, a
//! DMA slot) shows up in two places: the fabric snapshot, whose handle, buffer
//! and mapping totals must be exactly what they were before the first
//! iteration, and the driver's own counters, where every transmit slot must
//! come back and no ring error may appear. The traffic is real (one ARP
//! exchange per iteration), so it is in the packet capture too.

use alloc::format;
use alloc::string::String;

use user::messenger::fabric_stats;

use super::common::{arp_exchange, connect, fail, GATEWAY_IP};

/// Ring size used by every iteration.
const SLOTS: u32 = 16;

/// Run `iterations` attach/exchange/detach cycles; returns how many completed.
pub(super) fn run(iterations: u32) -> Result<u32, String> {
    let client = connect()?;
    let info = client.info().map_err(fail("info"))?;
    let mac: [u8; 6] = info
        .mac
        .as_slice()
        .try_into()
        .map_err(|_| String::from("the driver reported a bad MAC"))?;
    let before = fabric_stats().map_err(fail("fabric stats"))?;
    let stats_before = client.stats().map_err(fail("stats"))?;
    for round in 0..iterations {
        let what = |step: &str| format!("iteration {round}: {step}");
        let mut attachment = client.attach(SLOTS).map_err(fail(&what("attach")))?;
        arp_exchange(&client, &mut attachment, mac, GATEWAY_IP).map_err(|e| what(&e))?;
        client
            .detach(attachment.ring)
            .map_err(fail(&what("detach")))?;
        attachment.close();
    }
    let after = fabric_stats().map_err(fail("fabric stats"))?;
    let stats_after = client.stats().map_err(fail("stats"))?;
    let leaked = |name: &str, before: u64, after: u64| {
        if before == after {
            Ok(())
        } else {
            Err(format!(
                "{name} grew from {before} to {after} over {iterations} cycles"
            ))
        }
    };
    leaked("shared buffers", before.buffers, after.buffers)?;
    leaked(
        "buffer mappings",
        before.buffer_mappings,
        after.buffer_mappings,
    )?;
    leaked("handles", before.handles, after.handles)?;
    leaked("endpoints", before.endpoints, after.endpoints)?;
    if stats_after.ring_errors != stats_before.ring_errors {
        return Err(String::from(
            "the driver counted ring errors during the soak",
        ));
    }
    let sent = stats_after.tx_frames - stats_before.tx_frames;
    if sent < u64::from(iterations) {
        return Err(format!(
            "the driver transmitted {sent} frames for {iterations} requests"
        ));
    }
    Ok(iterations)
}
