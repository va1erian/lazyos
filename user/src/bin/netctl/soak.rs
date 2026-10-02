//! `netctl soak=<n>`: ping the gateway `n` times, renewing the DHCP lease
//! every tenth round and rebuilding the NIC attachment every twentieth.
//!
//! A leak of anything per round (a handle, a shared buffer) shows up in the
//! fabric snapshot's per-task usage of `netd` and `netdrv`, which must be
//! exactly what it was before the first round (per task, not global: other
//! services come and go and move the totals), and in the
//! stack's own counters, where every ping is answered, nothing is dropped for
//! length or ring trouble, and every reattachment is counted. The traffic is
//! real, so it is in the packet capture too.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use user::messenger::{fabric_stats, TaskUsage};
use user::{sys, sysinfo};

use super::common::{connect, fail, nap, wait_for_address};

const GATEWAY: [u8; 4] = [10, 0, 2, 2];

/// What `netd` and `netdrv` hold right now: `(name, usage)` per live task
/// with those names, in slot order.
fn holdings() -> Result<Vec<(String, TaskUsage)>, String> {
    let fabric = fabric_stats().map_err(fail("fabric stats"))?;
    let snapshot =
        sysinfo::snapshot().map_err(|code| format!("system snapshot: errno {}", -code))?;
    let mut out = Vec::new();
    for task in snapshot.live_tasks() {
        // A task is named after its program's basename (`netd`, `netdrv`),
        // whether the kernel or `init` started it.
        let name = task.name().to_ascii_lowercase();
        let kind = if name.starts_with("netdrv") {
            "netdrv"
        } else if name == "netd" || name.starts_with("netd.") {
            "netd"
        } else {
            continue;
        };
        if let Some(usage) = fabric.tasks.get(task.pid as usize) {
            out.push((String::from(kind), *usage));
        }
    }
    Ok(out)
}

/// Run `iterations` rounds; returns how many completed.
pub(super) fn run(iterations: u32) -> Result<u32, String> {
    let client = connect()?;
    wait_for_address(&client)?;
    let before = holdings()?;
    if before.len() != 2 {
        return Err(format!(
            "expected netd and netdrv in the task table, found {}",
            before.len()
        ));
    }
    let stats_before = client.stats().map_err(fail("stats"))?;
    let mut renewals = 0u64;
    let mut reattachments = 0u64;
    for round in 1..=iterations {
        let what = |step: &str| format!("round {round}: {step}");
        let echo = client
            .ping(GATEWAY, 1 + (round * 37) % 1400, 3000)
            .map_err(fail(&what("ping")))?;
        if echo.source != GATEWAY {
            return Err(what("the reply came from another address"));
        }
        if round % 10 == 0 {
            client.renew().map_err(fail(&what("renew")))?;
            wait_for_address(&client).map_err(|e| what(&e))?;
            renewals += 1;
        }
        if round % 20 == 0 {
            client.reattach().map_err(fail(&what("reattach")))?;
            reattachments += 1;
            let deadline = sys::clock() + 800;
            while !client
                .interfaces()
                .map_err(fail(&what("interfaces")))?
                .first()
                .is_some_and(|i| i.link)
                && sys::clock() < deadline
            {
                nap();
            }
        }
    }
    // Let the last reattachment and any late frames settle.
    for _ in 0..30 {
        nap();
    }
    let after = holdings()?;
    let stats_after = client.stats().map_err(fail("stats"))?;
    for ((name, was), (_, now)) in before.iter().zip(after.iter()) {
        let leaked = |what: &str, was: u64, now: u64| {
            if was == now {
                Ok(())
            } else {
                Err(format!(
                    "{name}: {what} went from {was} to {now} over {iterations} rounds"
                ))
            }
        };
        leaked("handles", was.handles, now.handles)?;
        leaked("shared buffers", was.buffers, now.buffers)?;
        leaked("buffer bytes", was.buffer_bytes, now.buffer_bytes)?;
    }
    let answered = stats_after.pings_answered - stats_before.pings_answered;
    if answered != u64::from(iterations) {
        return Err(format!("{answered} pings answered for {iterations} rounds"));
    }
    if stats_after.nic_resets - stats_before.nic_resets != reattachments {
        return Err(String::from("the NIC reattachments were not counted"));
    }
    if stats_after.leases - stats_before.leases != renewals {
        return Err(String::from("the lease renewals were not counted"));
    }
    if stats_after.rx_bad_length != stats_before.rx_bad_length
        || stats_after.tx_dropped != stats_before.tx_dropped
    {
        return Err(String::from(
            "frames were dropped for length or ring trouble",
        ));
    }
    Ok(iterations)
}
