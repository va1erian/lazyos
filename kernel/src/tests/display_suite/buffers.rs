//! The surface buffers clients draw into, through the `messenger` buffer
//! ops (`OP_BUFFER_CREATE`/`MAP`/`CLOSE`, issue #212 and the core plan's
//! M2): releasing a buffer, the errors around it, the compositor's own
//! screen buffer, and a create/close soak.

use super::*;
use crate::display::op;
use crate::tests::bufops;

const EBUSY: i64 = 16;
const ENOENT: i64 = 2;

/// Close drops the mapping and the quota charge; a second close and a bogus
/// handle are `-ENOENT`; a buffer the compositor retained (attach) outlives
/// the client's close.
pub fn close_buffer_releases() -> Result<(), String> {
    use crate::ipc::shared;
    crate::display::reset();
    scratch_task()?;
    bufops::in_space(|| {
        let baseline = shared::process_stats(task::current());
        let registry = shared::stats().buffers;
        let size = 3 * 4096u64;
        let (code, handle, va, reported) = bufops::create_raw(size);
        check!(code == 0, "buffer_create -> {code:#x}");
        check!(va != 0, "buffer_create returned no mapping");
        check!(reported == size, "buffer_create reported {reported} bytes");
        let (code, mapped, mapped_size) = bufops::map(handle);
        check!(
            code == 0 && mapped == va && mapped_size == size,
            "buffer_map of the creator's own buffer -> {code:#x} {mapped:#x} {mapped_size}"
        );
        let held = shared::process_stats(task::current());
        check!(
            held.buffers == baseline.buffers + 1 && held.bytes == baseline.bytes + size,
            "quota not charged: {held:?}"
        );

        check!(bufops::close(handle) == 0, "buffer_close failed");
        let after = shared::process_stats(task::current());
        check!(
            after.buffers == baseline.buffers && after.bytes == baseline.bytes,
            "close left the quota charge: {after:?}"
        );
        check!(shared::stats().buffers == registry, "close left the buffer");
        check!(
            bufops::close(handle) == bufops::failed(ENOENT),
            "second close not -ENOENT"
        );
        check!(
            bufops::close(0xdead_beef) == bufops::failed(ENOENT),
            "bogus handle not -ENOENT"
        );
        check!(
            bufops::map(0xdead_beef).0 == bufops::failed(ENOENT),
            "map of a bogus handle not -ENOENT"
        );

        // The compositor's reference (taken by attach) keeps the object alive.
        let (kept, _) = bufops::create(4096)?;
        let object_id = crate::ipc::handles::get_for_task(task::current(), kept)
            .map_err(|_| String::from("buffer handle not in the table"))?
            .object_id;
        shared::retain(object_id).map_err(|e| String::from(e.message()))?;
        check!(
            bufops::close(kept) == 0,
            "close of a retained buffer failed"
        );
        check!(
            shared::stats().buffers == registry + 1,
            "retained buffer was destroyed by the client's close"
        );
        shared::release(object_id);
        check!(
            shared::stats().buffers == registry,
            "compositor release left the buffer registered"
        );
        Ok(())
    })?;
    crate::display::reset();
    Ok(())
}

/// The bound compositor's own screen buffer is refused (`-EBUSY`); `unbind`
/// releases it.
pub fn close_buffer_refuses_screen() -> Result<(), String> {
    crate::display::reset();
    scratch_task()?;
    bufops::in_space(|| {
        let mut info = [0u64; crate::display::INFO_WORDS];
        let code = process::dispatch_for_test(12, op::BIND, info.as_mut_ptr() as u64, 0);
        check!(code == 0, "bind -> {code:#x}");
        let screen = info[crate::display::INFO_BUFFER];
        check!(
            bufops::close(screen) == bufops::failed(EBUSY),
            "screen buffer close not -EBUSY"
        );
        let code = process::dispatch_for_test(12, op::UNBIND, 0, 0);
        check!(code == 0, "unbind -> {code:#x}");
        Ok(())
    })?;
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
    bufops::in_space(|| {
        let baseline = shared::process_stats(task::current());
        let registry = shared::stats().buffers;
        // Handle numbers start at 0, so an empty slot is `None`, not 0.
        let mut live = [None::<u64>; 8];
        for round in 0..3000usize {
            let slot = round % live.len();
            if let Some(old) = live[slot] {
                check!(bufops::close(old) == 0, "close failed at round {round}");
            }
            let (handle, _) = bufops::create(4096 * (1 + (round % 3) as u64))?;
            live[slot] = Some(handle);
        }
        for handle in live.into_iter().flatten() {
            check!(bufops::close(handle) == 0, "final close failed");
        }
        let after = shared::process_stats(task::current());
        check!(
            after.buffers == baseline.buffers && after.bytes == baseline.bytes,
            "soak leaked quota: {after:?} vs {baseline:?}"
        );
        check!(shared::stats().buffers == registry, "soak leaked buffers");
        Ok(())
    })?;
    crate::display::reset();
    Ok(())
}
