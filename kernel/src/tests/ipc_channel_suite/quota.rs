//! The per-uid channel quota (audit #4): one user cannot take every slot of
//! the kernel's channel table and refuse every other `create`/`connect`.

use super::leaks::send_end;
use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::ipc::registry;
use crate::quota::{self, Resource};

/// The uid filling its share, and the one that must still get channels.
const GREEDY: u32 = 48_401;
const OTHER: u32 = 48_402;

/// Run the kernel task as `uid` (no capabilities).
fn as_user(uid: u32) {
    credentials::set(task::KERNEL_TASK, Cred::new(uid, uid, 0, 0, 0));
}

fn used(uid: u32) -> u64 {
    quota::usage(uid, Resource::Channels)
}

/// Run `body` from an empty channel table and ledger, and put root, the
/// table and the ledger back whatever it returns, so a failure here cannot
/// leave the rest of the suite running unprivileged.
fn isolated(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    fresh()?;
    quota::reset();
    let outcome = body();
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    fresh()?;
    registry::reset();
    quota::reset();
    outcome
}

/// Close both ends of every pair.
fn close_pairs(pairs: &[(u64, u64)]) -> Result<(), String> {
    for &(a, b) in pairs {
        channels::close_endpoint(a).map_err(reason)?;
        channels::close_endpoint(b).map_err(reason)?;
    }
    Ok(())
}

/// Create `count` pairs as the current uid.
fn create_pairs(count: u64) -> Result<Vec<(u64, u64)>, String> {
    (0..count)
        .map(|index| {
            channels::create().map_err(|error| format!("channel {index}: {}", error.message()))
        })
        .collect()
}

/// A regular uid gets exactly its share of channels, then `ChannelQuota`;
/// another uid still gets channels, a close makes room again, and closing
/// everything returns the whole charge.
pub fn channel_quota_caps_one_uid() -> Result<(), String> {
    isolated(|| {
        let share = quota::limit(GREEDY, Resource::Channels);
        check!(
            share > 0 && share < channels::MAX_CHANNELS as u64,
            "a regular uid may hold {share} of {} channels",
            channels::MAX_CHANNELS
        );
        as_user(GREEDY);
        let mut pairs = create_pairs(share)?;
        let refused = channels::create();
        check!(
            refused == Err(ChannelError::ChannelQuota),
            "channel {} past the share: {refused:?}",
            share + 1
        );
        check!(
            used(GREEDY) == share,
            "charged {}, expected {share}",
            used(GREEDY)
        );

        as_user(OTHER);
        let other = channels::create().map_err(reason)?;
        close_pairs(&[other])?;
        as_user(GREEDY);
        let last = pairs.pop().unwrap_or_default();
        close_pairs(&[last])?;
        pairs.push(channels::create().map_err(reason)?);
        close_pairs(&pairs)?;
        check!(used(GREEDY) == 0, "{} still charged", used(GREEDY));
        Ok(())
    })
}

/// A uid that filled its share with channels nobody can reach (an end in
/// its own inbox, its peer orphaned in a closed channel's inbox: no close
/// reclaims that shape) gets them back at its next create, as a full table
/// would.
pub fn channel_quota_reclaims_leaks() -> Result<(), String> {
    isolated(|| {
        let share = quota::limit(GREEDY, Resource::Channels);
        as_user(GREEDY);
        // Each round leaks x and frees y: `share - 1` leaks, then one live
        // pair fills the share.
        for round in 0..share - 1 {
            let (x0, x1) = channels::create().map_err(reason)?;
            let (y0, y1) = channels::create().map_err(reason)?;
            send_end(x1, x0).map_err(|error| format!("round {round}: {error}"))?;
            send_end(y0, x1).map_err(|error| format!("round {round}: {error}"))?;
            channels::close_endpoint(y1).map_err(reason)?;
            channels::close_endpoint(y0).map_err(reason)?;
        }
        let _held = channels::create().map_err(reason)?;
        check!(
            used(GREEDY) == share,
            "the leak and the live pair hold {} of {share}",
            used(GREEDY)
        );
        let next = channels::create();
        check!(
            next.is_ok(),
            "a uid full of unreachable channels was refused: {next:?}"
        );
        check!(
            used(GREEDY) == 2,
            "{} charged after the reclaim, expected the 2 live pairs",
            used(GREEDY)
        );
        Ok(())
    })
}

/// A channel's charge goes back to the uid that created it, even when the
/// creating task changed uid before the close.
pub fn channel_quota_returns_to_the_creator() -> Result<(), String> {
    isolated(|| {
        as_user(GREEDY);
        let pair = channels::create().map_err(reason)?;
        as_user(OTHER);
        close_pairs(&[pair])?;
        check!(used(GREEDY) == 0, "the creator kept {}", used(GREEDY));
        check!(used(OTHER) == 0, "the closer was charged {}", used(OTHER));
        Ok(())
    })
}

/// A connection is charged to its client, not to the task asking for it
/// (`messengerd` connects on a client's behalf).
pub fn channel_quota_connect_charges_the_client() -> Result<(), String> {
    isolated(|| {
        let (_listen, clients) = channels::create().map_err(reason)?;
        let entry = handles::get(clients).map_err(|e| String::from(e.message()))?;
        registry::register(
            task::KERNEL_TASK,
            "os.example.quota",
            entry.kind,
            entry.rights,
            entry.object_id,
            &[],
            0,
        )
        .map_err(|e| String::from(e.message()))?;
        let client = task::spawn_fork().map_err(String::from)?;
        credentials::set(client, Cred::new(GREEDY, GREEDY, 0, 0, 0));
        let proxy_before = used(0);
        let connected = crate::ipc::connect::connect(client, "os.example.quota");
        let (client_used, proxy_after) = (used(GREEDY), used(0));
        task::harness::finish(client, 0);
        let _ = task::reap_child();
        check!(connected.is_ok(), "connect failed: {connected:?}");
        check!(client_used == 1, "the client was charged {client_used}");
        check!(
            proxy_after == proxy_before,
            "the proxy was charged {} channels",
            proxy_after - proxy_before
        );
        Ok(())
    })
}

/// Soak: creating and closing many channels as a regular uid leaves no
/// charge and no channel behind.
pub fn channel_quota_soak() -> Result<(), String> {
    isolated(|| {
        let live = channels::counts().channels;
        as_user(GREEDY);
        for round in 0..10_000u32 {
            let pair = channels::create().map_err(|e| format!("round {round}: {}", e.message()))?;
            close_pairs(&[pair])?;
        }
        check!(used(GREEDY) == 0, "{} channels still charged", used(GREEDY));
        let left = channels::counts().channels;
        check!(left == live, "{} channels leaked", left - live);
        Ok(())
    })
}
