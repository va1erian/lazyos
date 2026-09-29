//! `close_buffer` (display op 6, issue #212): releasing a buffer made by
//! `create_buffer`, the errors around it, and a create/close soak.

use super::*;
use crate::display::op;

const EBADF: i64 = 9;
const EBUSY: i64 = 16;

/// `create_buffer` through the syscall gate: `(handle, va)`.
fn create(size: u64) -> Result<(u64, u64), String> {
    let mut words = [0u64; 3];
    let code = process::dispatch_for_test(12, op::CREATE_BUFFER, size, words.as_mut_ptr() as u64);
    check!(code == 0, "create_buffer({size}) -> {code:#x}");
    Ok((words[0], words[1]))
}

fn close(handle: u64) -> u64 {
    process::dispatch_for_test(12, op::CLOSE_BUFFER, handle, 0)
}

/// Close drops the mapping and the quota charge; a second close and a bogus
/// handle are `-EBADF`; a buffer the compositor retained (attach) outlives
/// the client's close.
pub fn close_buffer_releases() -> Result<(), String> {
    use crate::ipc::shared;
    crate::display::reset();
    scratch_task()?;
    let baseline = shared::process_stats(task::current());
    let registry = shared::stats().buffers;
    let size = 3 * 4096u64;
    let (handle, va) = create(size)?;
    check!(va != 0, "create_buffer returned no mapping");
    let held = shared::process_stats(task::current());
    check!(
        held.buffers == baseline.buffers + 1 && held.bytes == baseline.bytes + size,
        "quota not charged: {held:?}"
    );

    check!(close(handle) == 0, "close_buffer failed");
    let after = shared::process_stats(task::current());
    check!(
        after.buffers == baseline.buffers && after.bytes == baseline.bytes,
        "close left the quota charge: {after:?}"
    );
    check!(shared::stats().buffers == registry, "close left the buffer");
    check!(close(handle) == failed(EBADF), "second close not -EBADF");
    check!(
        close(0xdead_beef) == failed(EBADF),
        "bogus handle not -EBADF"
    );

    // The compositor's reference (taken by attach) keeps the object alive.
    let (kept, _) = create(4096)?;
    let object_id = crate::ipc::handles::get_for_task(task::current(), kept)
        .map_err(|_| String::from("buffer handle not in the table"))?
        .object_id;
    shared::retain(object_id).map_err(|e| String::from(e.message()))?;
    check!(close(kept) == 0, "close of a retained buffer failed");
    check!(
        shared::stats().buffers == registry + 1,
        "retained buffer was destroyed by the client's close"
    );
    shared::release(object_id);
    check!(
        shared::stats().buffers == registry,
        "compositor release left the buffer registered"
    );
    crate::display::reset();
    Ok(())
}

/// The bound compositor's own screen buffer is refused (`-EBUSY`); `unbind`
/// releases it.
pub fn close_buffer_refuses_screen() -> Result<(), String> {
    crate::display::reset();
    scratch_task()?;
    let mut info = [0u64; crate::display::INFO_WORDS];
    let code = process::dispatch_for_test(12, op::BIND, info.as_mut_ptr() as u64, 0);
    check!(code == 0, "bind -> {code:#x}");
    let screen = info[crate::display::INFO_BUFFER];
    check!(
        close(screen) == failed(EBUSY),
        "screen buffer close not -EBUSY"
    );
    let code = process::dispatch_for_test(12, op::UNBIND, 0, 0);
    check!(code == 0, "unbind -> {code:#x}");
    crate::display::reset();
    Ok(())
}

/// Thousands of create/close cycles with a bounded live set: no quota or
/// registry growth, and the 64-buffer cap is never hit. Mapping ranges are
/// recycled and empty page tables reclaimed (issue #237), so the run leaves
/// the frame-leak check of the hardening soak that follows undisturbed.
pub fn close_buffer_soak() -> Result<(), String> {
    use crate::ipc::shared;
    crate::display::reset();
    scratch_task()?;
    let baseline = shared::process_stats(task::current());
    let registry = shared::stats().buffers;
    // Handle numbers start at 0, so an empty slot is `None`, not 0.
    let mut live = [None::<u64>; 8];
    for round in 0..3000usize {
        let slot = round % live.len();
        if let Some(old) = live[slot] {
            check!(close(old) == 0, "close failed at round {round}");
        }
        let (handle, _) = create(4096 * (1 + (round % 3) as u64))?;
        live[slot] = Some(handle);
    }
    for handle in live.into_iter().flatten() {
        check!(close(handle) == 0, "final close failed");
    }
    let after = shared::process_stats(task::current());
    check!(
        after.buffers == baseline.buffers && after.bytes == baseline.bytes,
        "soak leaked quota: {after:?} vs {baseline:?}"
    );
    check!(shared::stats().buffers == registry, "soak leaked buffers");
    crate::display::reset();
    Ok(())
}
