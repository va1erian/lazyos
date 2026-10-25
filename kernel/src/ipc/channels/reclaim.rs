//! Reclaiming channel ends nobody can reach (audit H8).
//!
//! A channel end is *moved* by sending it, so a message in some inbox can be
//! the only thing that names an end. When that inbox belongs to the end
//! itself (an end sent over its own channel) or to a ring of channels that
//! carry each other's ends, no handle can ever receive the message: the ends
//! are alive for the kernel and dead for everyone, and the registry's slots
//! (and their owner's channel quota) fill up one leaked round at a time.
//!
//! A side is live while a handle names it, a name publishes it, the kernel
//! holds it, or a message in the inbox of a *live* side carries it (whoever
//! holds that side can receive the message and so gets the end). Everything
//! else is unreachable, and closing it is what its peer's `PeerDied` and the
//! registry slot are waiting for.
//!
//! The roots are read before the registry lock is taken, which is sound on
//! one CPU: a syscall runs to its end or parks, so no end can move between a
//! table and an inbox in between (see `docs/architecture/smp-audit.md`).

use alloc::collections::BTreeSet;

use super::*;

/// Close every channel end that nothing can reach any more; returns how many
/// ends were closed.
///
/// Run when a close leaves its peer without a handle ([`close_endpoint_for`])
/// and when the registry or the creator's channel quota is full ([`create`]),
/// so a leak costs a slot only until the next such event. Roots are read
/// before the registry lock is taken (the handle tables and the name table
/// are never held under it), and the closes run with it released, like every
/// close.
pub(super) fn reclaim_unreachable() -> usize {
    let mut live: BTreeSet<u64> = handles::channel_objects().into_iter().collect();
    live.extend(crate::ipc::registry::object_ids());
    let dead = {
        let channels = CHANNELS.lock();
        mark(&channels, &mut live);
        unreachable_sides(&channels, &live)
    };
    let mut closed = 0;
    for (channel_id, side) in dead {
        // Closing one end may have removed or closed the next (a channel
        // whose two ends were both unreachable): `close_side` is idempotent.
        let orphans = close_side(channel_id, side);
        close_orphans(orphans);
        closed += 1;
    }
    closed
}

/// Extend `live` with every end carried by a message waiting for a live side.
fn mark(channels: &Registry, live: &mut BTreeSet<u64>) {
    let mut work: Vec<u64> = live.iter().copied().collect();
    while let Some(object) = work.pop() {
        let (channel_id, side) = split_object_id(object);
        let Some(channel) = channels.get(channel_id) else {
            continue;
        };
        for message in &channel.endpoints[side].inbox {
            for carried in channel_objects(&message.objects) {
                if live.insert(carried) {
                    work.push(carried);
                }
            }
        }
    }
}

/// The open sides of `channels` outside `live`. A side the kernel holds (an
/// interrupt channel's sending side) has no handle by design and is live.
fn unreachable_sides(channels: &Registry, live: &BTreeSet<u64>) -> Vec<(u64, usize)> {
    let mut dead = Vec::new();
    for channel in channels.iter() {
        for side in 0..2 {
            let endpoint = &channel.endpoints[side];
            if !endpoint.closed
                && !endpoint.kernel_held
                && !live.contains(&object_id(channel.id, side))
            {
                dead.push((channel.id, side));
            }
        }
    }
    dead
}

/// Whether `side` of `channel_id` is open and has a channel end waiting in
/// its inbox: the cheap test that decides if the close of its peer is worth
/// a [`reclaim_unreachable`] pass. Both leak shapes (an end sent over its own
/// channel, a ring of channels carrying each other's ends) leave the end in
/// the survivor's inbox; anything rarer is found when the registry, or
/// its creator's channel quota, fills.
pub(super) fn holds_moved_ends(channel_id: u64, side: usize) -> bool {
    CHANNELS.lock().get(channel_id).is_some_and(|channel| {
        let endpoint = &channel.endpoints[side];
        !endpoint.closed
            && !endpoint.kernel_held
            && endpoint
                .inbox
                .iter()
                .any(|message| channel_objects(&message.objects).next().is_some())
    })
}
