//! Malformed-pointer rejection, late replies, the bootstrap claim
//! flow, and the `OP_STATS` fabric snapshot.

use super::*;

/// Every malformed pointer or length returns an error and leaves the task
/// runnable: no kernel fault, no panic, no side effect.
pub fn syscall_bad_pointer() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let request = parcel(7, flags::SYNC, "x")?;
        write_bytes(REQUEST, &request);

        // 1. An unmapped parcel pointer fails cleanly and enqueues nothing.
        let args = MsgArgs {
            handle: created.value,
            parcel_ptr: 0xdead_0000,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_CALL_BEGIN, &args);
        check!(
            code == failed(errno::EFAULT),
            "unmapped parcel -> {code:#x}"
        );
        check!(
            result.status == -errno::EFAULT,
            "bad-pointer status is {}",
            result.status
        );
        check!(
            channels::stats().queued == 0,
            "a bad pointer enqueued a message"
        );

        // 2. An unmapped args block: the result block cannot be trusted
        //    either, so the return register is the only report.
        let code = process::dispatch_for_test(5, OP_CREATE_PAIR, 0xdead_0000, RESULT);
        check!(code == failed(errno::EFAULT), "unmapped args -> {code:#x}");

        // 3. An unmapped result block is refused before the op runs.
        let code = process::dispatch_for_test(5, OP_CREATE_PAIR, ARGS, 0xdead_0000);
        check!(
            code == failed(errno::EFAULT),
            "unmapped result -> {code:#x}"
        );

        // 4. An oversized parcel length is refused before any copy.
        let args = MsgArgs {
            handle: created.value,
            parcel_ptr: REQUEST,
            parcel_len: (libmessenger::MAX_PARCEL_BYTES + 1) as u64,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CALL_BEGIN, &args);
        check!(
            code == failed(errno::E2BIG),
            "oversized parcel -> {code:#x}"
        );

        // 5. A garbage parcel is caught by the codec, not the channel.
        write_bytes(REQUEST, &[0u8; 48]);
        let args = MsgArgs {
            handle: created.value,
            parcel_ptr: REQUEST,
            parcel_len: 48,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CALL_BEGIN, &args);
        check!(code == failed(errno::EINVAL), "garbage parcel -> {code:#x}");

        check!(
            task::harness::state(task::KERNEL_TASK) == Some(TaskState::Runnable),
            "the task did not survive the bad pointers"
        );
        check!(
            channels::stats().calls == 0,
            "a refused call registered a transaction"
        );
        Ok(())
    })
}

/// A reply to a transaction whose caller timed out, canceled or died is
/// `-ENOENT` through the syscall. Every userspace service loop relies on
/// that exact code to tell "the caller hung up" (keep serving) from a real
/// failure; `Endpoint::reply_or_drop` swallows `-ENOENT` and nothing else.
pub fn late_reply_is_enoent() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let request = parcel(7, flags::SYNC, "slow")?;
        let late = parcel(8, 0, "late")?;
        write_bytes(REPLY_BUF, &late);
        let reply_args = |txn: u64| MsgArgs {
            txn_id: txn,
            parcel_ptr: REPLY_BUF,
            parcel_len: late.len() as u64,
            ..MsgArgs::default()
        };

        // Expired: the deadline passed before the server answered.
        let deadline = task::ticks() + 10;
        let txn =
            channels::begin_call(created.value, 7, &request, Some(deadline)).map_err(reason)?;
        channels::expire_deadlines(deadline);
        let (code, _) = syscall(OP_REPLY, &reply_args(txn));
        check!(
            code == failed(errno::ENOENT),
            "a reply to an expired call -> {code:#x}"
        );
        check!(
            channels::await_reply(txn) == Err(channels::Error::TimedOut),
            "the expired call did not report TimedOut"
        );

        // Canceled: the caller gave up.
        let txn = channels::begin_call(created.value, 7, &request, None).map_err(reason)?;
        channels::cancel(txn).map_err(reason)?;
        let (code, _) = syscall(OP_REPLY, &reply_args(txn));
        check!(
            code == failed(errno::ENOENT),
            "a reply to a canceled call -> {code:#x}"
        );
        check!(
            channels::await_reply(txn) == Err(channels::Error::Canceled),
            "the canceled call did not report Canceled"
        );

        // Dead peer: the caller's endpoint closed while the call was open.
        let txn = channels::begin_call(created.value, 7, &request, None).map_err(reason)?;
        channels::close_endpoint(created.value).map_err(reason)?;
        let (code, _) = syscall(OP_REPLY, &reply_args(txn));
        check!(
            code == failed(errno::ENOENT),
            "a reply to a dead caller's call -> {code:#x}"
        );
        task::wake_task(task::KERNEL_TASK);
        let _ = task::harness::take_wake_reason(task::KERNEL_TASK);
        Ok(())
    })
}

/// The bootstrap flow: one kernel-created pair, the client end claimed by
/// a userspace task exactly once, the service end served by the kernel
/// stub, and a reply that round-trips.
pub fn bootstrap_claim() -> Result<(), String> {
    fresh()?;
    syscalls::bootstrap::create().map_err(to_string)?;
    check!(
        syscalls::bootstrap::service_handle().is_some(),
        "create left no service handle"
    );

    let child = task::spawn_fork().map_err(to_string)?;
    task::harness::switch_current(child);
    let client =
        syscalls::bootstrap::claim_client().map_err(|code| format!("claim failed: {code}"))?;
    check!(
        handles::count_for_task(child) == 1,
        "child holds {} handles, expected 1",
        handles::count_for_task(child)
    );
    check!(
        syscalls::bootstrap::claim_client() == Err(errno::EBUSY),
        "the client end was claimed twice"
    );

    // The service end stays kernel-side; the stub echoes the client.
    let request = parcel(1, flags::SYNC, "bootstrap")?;
    channels::send(client, &request).map_err(reason)?;
    task::harness::switch_current(task::KERNEL_TASK);
    check!(
        syscalls::bootstrap::claim_client() == Err(errno::EPERM),
        "the kernel task claimed the client end"
    );
    check!(
        syscalls::bootstrap::stub_serve().map_err(reason)?,
        "the stub found no request"
    );
    check!(
        !syscalls::bootstrap::stub_serve().map_err(reason)?,
        "the stub served the same request twice"
    );

    task::harness::switch_current(child);
    let reply = channels::recv(client, None).map_err(reason)?;
    check!(
        reply.bytes == request,
        "the stub reply differs from the request"
    );

    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(child, 0);
    check!(
        task::reap_child().is_some(),
        "the bootstrap child was not reapable"
    );
    Ok(())
}

/// `OP_STATS` serves the versioned `FabricStats` snapshot when the caller
/// offers a snapshot-sized buffer, keeps the 64-byte v1 counters for small
/// buffers and for a per-channel handle, and `OP_TOTALS` always returns
/// the compact global counters.
pub fn syscall_fabric_stats() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");

        // Version 2: a snapshot-sized buffer selects the rich block.
        write_bytes(STATS_BUF, &vec![0u8; FabricStats::SIZE]);
        let args = MsgArgs {
            buf_ptr: STATS_BUF,
            buf_cap: FabricStats::SIZE as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_STATS, &args);
        check!(
            code == 0 && result.bytes as usize == FabricStats::SIZE,
            "fabric stats -> {code:#x}, {} bytes",
            result.bytes
        );
        let snap = FabricStats::from_bytes(&read_bytes(STATS_BUF, FabricStats::SIZE))
            .ok_or("bad fabric stats block")?;
        check!(
            snap.version == FABRIC_STATS_VERSION,
            "snapshot version is {}",
            snap.version
        );
        check!(
            snap.channels == 1 && snap.endpoints == 2,
            "snapshot counts are {snap:?}"
        );
        check!(
            snap.handles == 2 && snap.handles_per_task[task::current()] == 2,
            "snapshot lost the pair handles: {snap:?}"
        );
        check!(
            snap.audit_last_hash == audit::last_hash(),
            "snapshot hash is not the live chain head"
        );

        // Version 1, per channel: a 64-byte buffer and a handle keep the
        // compact counters.
        let args = MsgArgs {
            handle: created.value,
            buf_ptr: STATS_BUF,
            buf_cap: MsgStats::SIZE as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_STATS, &args);
        check!(
            code == 0 && result.bytes as usize == MsgStats::SIZE,
            "channel stats -> {code:#x}, {} bytes",
            result.bytes
        );

        // The dedicated totals op serves the same compact shape globally.
        let args = MsgArgs {
            buf_ptr: STATS_BUF,
            buf_cap: MsgStats::SIZE as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_TOTALS, &args);
        check!(
            code == 0 && result.bytes as usize == MsgStats::SIZE,
            "totals -> {code:#x}, {} bytes",
            result.bytes
        );
        let totals = MsgStats::from_bytes(&read_bytes(STATS_BUF, MsgStats::SIZE))
            .ok_or("bad totals block")?;
        check!(
            totals.calls == 0 && totals.replies == 0 && totals.drops == 0,
            "totals after pair creation: {totals:?}"
        );
        Ok(())
    })
}
