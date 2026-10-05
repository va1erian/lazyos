//! The requester's half of a provider slot: one request through the slot,
//! waiting for the daemon in slices (see the module docs of [`super`]).

use core::sync::atomic::{AtomicU32, Ordering};

use fused::wire::{Reply, Request, MAX_PAYLOAD};

use super::{
    now, park, test_hook, Phase, Slot, State, DEAD_AFTER_TIMEOUTS, REQUEST_TICKS, SLICE_TICKS,
    SLOTS,
};
use crate::fs::vfs::FsError;
use crate::task::{self, wait::WaitQueue};

/// Send `request` (its tag is filled in here) with a payload made of
/// `parts` to provider `index` of registration `epoch`, and wait for the
/// reply. Returns the reply and how many bytes of `out` its data filled.
pub(super) fn transact(
    index: usize,
    epoch: u64,
    mut request: Request,
    parts: &[&[u8]],
    out: &mut [u8],
) -> Result<(Reply, usize), FsError> {
    let slot = SLOTS.get(index).ok_or(FsError::Io)?;
    if !task::relax::can_block() {
        report_unparkable();
        return Err(FsError::Io);
    }
    let payload_len: usize = parts.iter().map(|part| part.len()).sum();
    if payload_len > MAX_PAYLOAD {
        return Err(FsError::Invalid);
    }
    let deadline = now() + REQUEST_TICKS;
    // Take the request slot.
    let tag = loop {
        {
            let mut state = slot.state.lock();
            if !current(&state, epoch) {
                return Err(FsError::Io);
            }
            if !state.busy {
                state.busy = true;
                state.next_tag += 1;
                request.tag = state.next_tag;
                state.request = request;
                let mut at = 0;
                for part in parts {
                    state.bounce[at..at + part.len()].copy_from_slice(part);
                    at += part.len();
                }
                state.payload_len = payload_len;
                state.reply_cap = out.len().min(MAX_PAYLOAD);
                state.phase = Phase::Queued;
                state.stats.requests += 1;
                break request.tag;
            }
        }
        if now() >= deadline {
            return Err(FsError::Io);
        }
        wait(slot, &slot.idle, deadline);
    };
    slot.work.notify_all();
    let result = await_reply(slot, epoch, tag, deadline, out);
    slot.idle.notify_one();
    if result.is_err() {
        slot.state.lock().stats.errors += 1;
    }
    result
}

/// Whether `state` is still the live provider of registration `epoch`.
fn current(state: &State, epoch: u64) -> bool {
    state.registered && state.alive && state.epoch == epoch
}

/// Wait for request `tag`, then release the slot.
fn await_reply(
    slot: &Slot,
    epoch: u64,
    tag: u64,
    deadline: u64,
    out: &mut [u8],
) -> Result<(Reply, usize), FsError> {
    loop {
        {
            let mut state = slot.state.lock();
            // The slot is never freed while this request holds it busy.
            debug_assert!(state.request.tag == tag && state.epoch == epoch);
            if let Phase::Done(result) = state.phase {
                let reply = state.reply;
                // `reply` refused data longer than the request's room.
                let len = (reply.data_len as usize).min(state.reply_cap);
                if result.is_ok() {
                    out[..len].copy_from_slice(&state.bounce[..len]);
                }
                state.timeouts = 0;
                release(&mut state);
                return result.map(|()| (reply, len));
            }
            if !state.alive || !task::live(state.owner) {
                state.alive = false;
                release(&mut state);
                super::REAP.store(true, Ordering::Release);
                return Err(FsError::Io);
            }
            if now() >= deadline {
                state.timeouts += 1;
                state.stats.timeouts += 1;
                if state.timeouts >= DEAD_AFTER_TIMEOUTS {
                    state.alive = false;
                    super::REAP.store(true, Ordering::Release);
                    serial_println!(
                        "fuse: /mnt/{}: provider stopped answering; dead",
                        state.name
                    );
                }
                release(&mut state);
                return Err(FsError::Io);
            }
        }
        wait(slot, &slot.done, (now() + SLICE_TICKS).min(deadline));
    }
}

/// Park until `deadline`, or let the test's fake daemon run instead.
fn wait(slot: &Slot, queue: &WaitQueue, deadline: u64) {
    if test_hook::serve(slot.index) {
        return;
    }
    park(queue, deadline);
}

/// Free the request slot: a late reply to this tag is now stale.
fn release(state: &mut State) {
    state.phase = Phase::Idle;
    state.busy = false;
}

/// A request from a context that cannot park: say so once.
fn report_unparkable() {
    static SEEN: AtomicU32 = AtomicU32::new(0);
    if SEEN.fetch_add(1, Ordering::Relaxed) == 0 {
        serial_println!("fuse: request from a context that cannot sleep; failed");
    }
}
