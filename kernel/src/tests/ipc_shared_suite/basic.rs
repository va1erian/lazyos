//! Buffer create/write/read, quota accounting, and `SHARE_ONLY`
//! isolation.

use super::*;

/// Create maps the buffer into the creator, the mapping round-trips bytes
/// and never copies, and close returns the frames to the allocator.
pub fn buffer_create_write_read() -> Result<(), String> {
    fresh()?;
    let size = 3 * 4096;
    let handle =
        shared::create(size, shared::flags::READ | shared::flags::WRITE).map_err(buffer_reason)?;
    let info = shared::info(handle).map_err(buffer_reason)?;
    check!(info.size == size, "buffer size is {}", info.size);
    check!(
        info.frames == 3,
        "buffer has {} frames, expected 3",
        info.frames
    );
    check!(info.refs == 1, "creator references are {}", info.refs);
    check!(
        info.mappings == 1,
        "creator mappings are {}, expected 1",
        info.mappings
    );

    let va = shared::map(handle).map_err(buffer_reason)?;
    check!(
        shared::map(handle).map_err(buffer_reason)? == va,
        "map is not idempotent for one task"
    );
    let mut frames = Vec::new();
    for page in 0..3u64 {
        let pte = raw_entry(mem::kernel_table(), va + page * 4096)
            .ok_or_else(|| format!("buffer page {page} is not mapped"))?;
        check!(
            pte & PTE_WRITABLE != 0,
            "buffer page {page} is not writable: {pte:#x}"
        );
        frames.push(pte & PTE_ADDR);
    }
    for offset in 0..size as usize {
        // Safety: the buffer is mapped read/write at `va` for `size` bytes.
        unsafe {
            (va as *mut u8)
                .add(offset)
                .write_volatile(pattern_byte(0x5a, offset))
        };
    }
    for offset in (0..size as usize).step_by(37) {
        // Safety: as above.
        let got = unsafe { (va as *const u8).add(offset).read_volatile() };
        check!(
            got == pattern_byte(0x5a, offset),
            "byte {offset} is {got:#x} (mapping corrupted)"
        );
    }

    let stats = shared::stats();
    check!(
        stats.buffers == 1 && stats.bytes == size && stats.mappings == 1,
        "registry stats after create: {stats:?}"
    );
    let process = shared::process_stats(task::current());
    check!(
        process.bytes == size && process.buffers == 1,
        "process stats after create: {process:?}"
    );

    shared::close(handle).map_err(buffer_reason)?;
    check!(
        raw_entry(mem::kernel_table(), va).is_none(),
        "close left the mapping in place"
    );
    for (page, frame) in frames.iter().enumerate() {
        check!(
            mem::frame_refcount(PhysAddr::new(*frame)) == 0,
            "close leaked frame {page} ({frame:#x})"
        );
    }
    check!(
        shared::stats().buffers == 0,
        "close left the buffer in the registry"
    );
    check!(
        shared::info(handle) == Err(BufferError::InvalidHandle),
        "a closed buffer handle still resolves"
    );
    Ok(())
}

/// The per-process count and byte quotas are enforced and released as
/// buffers close.
pub fn buffer_quota() -> Result<(), String> {
    fresh()?;
    // Count quota: fill with small buffers, then one more is refused.
    let small = 4096u64;
    let mut handles = Vec::new();
    for index in 0..shared::MAX_BUFFERS_PER_PROCESS {
        let handle = shared::create(small, shared::flags::READ | shared::flags::WRITE)
            .map_err(|error| format!("buffer {index}: {}", error.message()))?;
        handles.push(handle);
    }
    check!(
        shared::create(small, shared::flags::READ) == Err(BufferError::Quota),
        "the buffer-count quota was not enforced"
    );
    for handle in handles.drain(..) {
        shared::close(handle).map_err(buffer_reason)?;
    }
    check!(
        shared::process_stats(task::current()).buffers == 0,
        "closing did not release the count quota"
    );

    // Byte quota: one buffer at the limit, then any more is refused.
    let handle = shared::create(
        shared::max_bytes_per_process(),
        shared::flags::READ | shared::flags::WRITE,
    )
    .map_err(buffer_reason)?;
    check!(
        shared::create(small, shared::flags::READ) == Err(BufferError::Quota),
        "the buffer-byte quota was not enforced"
    );
    shared::close(handle).map_err(buffer_reason)?;
    check!(
        shared::process_stats(task::current()).bytes == 0,
        "closing did not release the byte quota"
    );
    Ok(())
}

/// A `SHARE_ONLY` buffer is mapped for its creator but the kernel refuses
/// to map it in a receiver that got the handle.
pub fn buffer_share_only_not_mappable() -> Result<(), String> {
    fresh()?;
    let creator = task::current();
    let child = spawn_receiver()?;
    let (client, child_server) = channel_to(child)?;
    let handle = shared::create(4096, shared::flags::READ | shared::flags::SHARE_ONLY)
        .map_err(buffer_reason)?;
    let creator_va = shared::map(handle).map_err(buffer_reason)?;
    check!(
        raw_entry(mem::kernel_table(), creator_va).is_some(),
        "the creator's SHARE_ONLY mapping is missing"
    );

    let bytes = parcel_with_transfers(1, "key material", vec![handle], Vec::new())?;
    channels::send(client, &bytes).map_err(channel_reason)?;
    check!(
        handles::get(handle) == Err(HandleError::InvalidHandle),
        "the transfer did not move the sender's handle"
    );

    task::harness::switch_current(child);
    let message = channels::try_recv(child_server)
        .map_err(channel_reason)?
        .ok_or("the transferred message is missing")?;
    check!(
        message.handles.len() == 1,
        "delivered {} handles, expected 1",
        message.handles.len()
    );
    check!(
        shared::map(message.handles[0]) == Err(BufferError::ShareOnly),
        "a receiver mapped a SHARE_ONLY buffer"
    );

    // Cleanup: the buffer still has the receiver's reference.
    shared::reset();
    handles::reset_for_task(child);
    task::harness::switch_current(creator);
    channels::reset();
    reap(child)?;
    Ok(())
}
