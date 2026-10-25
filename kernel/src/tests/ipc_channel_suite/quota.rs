//! The per-uid channel quota (audit #4): one user cannot take every slot of
//! the kernel's channel table and refuse every other `create`/`connect`.

use super::*;
use crate::ipc::credentials::{self, Cred};
use crate::quota::{self, Resource};

/// The uid filling its share, and the one that must still get channels.
const GREEDY: u32 = 48_401;
const OTHER: u32 = 48_402;

/// Run the kernel task as `uid` (no capabilities).
fn as_user(uid: u32) {
    credentials::set(task::KERNEL_TASK, Cred::new(uid, uid, 0, 0, 0));
}

/// Back to root with an empty ledger and channel table.
fn restore() -> Result<(), String> {
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    fresh()?;
    quota::reset();
    Ok(())
}

/// Close both ends of every pair.
fn close_pairs(pairs: &[(u64, u64)]) -> Result<(), String> {
    for &(a, b) in pairs {
        channels::close_endpoint(a).map_err(reason)?;
        channels::close_endpoint(b).map_err(reason)?;
    }
    Ok(())
}

/// A regular uid gets exactly its share of channels, then `ChannelQuota`;
/// another uid still gets channels, a close makes room again, and closing
/// everything returns the whole charge.
pub fn channel_quota_caps_one_uid() -> Result<(), String> {
    restore()?;
    let share = quota::limit(GREEDY, Resource::Channels);
    check!(
        share > 0 && share < channels::MAX_CHANNELS as u64,
        "a regular uid may hold {share} of {} channels",
        channels::MAX_CHANNELS
    );
    as_user(GREEDY);
    let mut pairs = Vec::new();
    for index in 0..share {
        match channels::create() {
            Ok(pair) => pairs.push(pair),
            Err(error) => {
                close_pairs(&pairs)?;
                restore()?;
                return Err(format!("channel {index} of {share}: {}", error.message()));
            }
        }
    }
    let refused = channels::create();
    let used = quota::usage(GREEDY, Resource::Channels);

    as_user(OTHER);
    let other = channels::create();
    as_user(GREEDY);
    if let Ok(pair) = other {
        close_pairs(&[pair])?;
    }
    let last = pairs.pop().unwrap_or_default();
    close_pairs(&[last])?;
    let again = channels::create();
    if let Ok(pair) = again {
        pairs.push(pair);
    }
    close_pairs(&pairs)?;
    let after = quota::usage(GREEDY, Resource::Channels);
    restore()?;

    check!(
        refused == Err(ChannelError::ChannelQuota),
        "channel {} past the share: {refused:?}",
        share + 1
    );
    check!(
        used == share,
        "the uid was charged {used}, expected {share}"
    );
    check!(other.is_ok(), "another uid was refused: {other:?}");
    check!(again.is_ok(), "a close did not make room: {again:?}");
    check!(
        after == 0,
        "{after} channels still charged after closing all"
    );
    Ok(())
}

/// Soak: creating and closing many channels as a regular uid leaves no
/// charge and no channel behind.
pub fn channel_quota_soak() -> Result<(), String> {
    restore()?;
    let live = channels::counts().channels;
    as_user(GREEDY);
    for round in 0..10_000u32 {
        let pair = channels::create().map_err(|error| {
            credentials::set(task::KERNEL_TASK, Cred::ROOT);
            format!("round {round}: {}", error.message())
        })?;
        close_pairs(&[pair])?;
    }
    let charged = quota::usage(GREEDY, Resource::Channels);
    let left = channels::counts().channels;
    restore()?;
    check!(charged == 0, "{charged} channels still charged");
    check!(left == live, "{} channels leaked", left - live);
    Ok(())
}
