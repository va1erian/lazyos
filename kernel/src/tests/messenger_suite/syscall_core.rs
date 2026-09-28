//! The full native syscall path: echo call/reply, blocking call
//! through the timer gate, and the ACL hook.

use super::*;

/// The full syscall path: a synchronous echo call and reply, byte for byte,
/// plus one-way send, cancel, stats, and close. Prints `MSG:ECHO:PASS` so
/// CI can grep the round trip.
pub fn syscall_echo() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        // 1. Create the pair through the syscall; both handles land in the
        //    calling (kernel) task's table.
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0 && created.status == 0, "create_pair -> {code:#x}");
        let (client, server) = (created.value, created.aux);
        check!(client != server, "create_pair reused handle {client}");

        // 2. Begin a synchronous call with a "ping" request parcel.
        let request = parcel(7, flags::SYNC, "ping")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            handle: client,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, begun) = syscall(OP_CALL_BEGIN, &args);
        check!(code == 0, "call_begin -> {code:#x}");
        let txn = begun.value;
        check!(txn != 0, "call_begin returned transaction 0");

        // 3. The server receives exactly the request bytes.
        let args = MsgArgs {
            handle: server,
            buf_ptr: RECV_BUF,
            buf_cap: 4096,
            ..MsgArgs::default()
        };
        let (code, received) = syscall(OP_RECV, &args);
        check!(code == 0, "recv -> {code:#x}");
        check!(
            received.value == txn,
            "recv transaction is {} (expected {txn})",
            received.value
        );
        check!(
            received.aux == task::current() as u64,
            "recv sender is {} (expected {})",
            received.aux,
            task::current()
        );
        let got = read_bytes(RECV_BUF, received.bytes as usize);
        check!(got == request, "request bytes changed in flight");
        check!(string_field(&got)? == "ping", "request payload changed");

        // 4. Reply with "pong"; the caller awaits the exact reply parcel.
        let reply = parcel(8, 0, "pong")?;
        write_bytes(REPLY_BUF, &reply);
        let args = MsgArgs {
            txn_id: txn,
            parcel_ptr: REPLY_BUF,
            parcel_len: reply.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_REPLY, &args);
        check!(code == 0, "reply -> {code:#x}");

        let args = MsgArgs {
            txn_id: txn,
            buf_ptr: RECV_BUF,
            buf_cap: 4096,
            ..MsgArgs::default()
        };
        let (code, awaited) = syscall(OP_CALL_AWAIT, &args);
        check!(code == 0, "call_await -> {code:#x}");
        let got = read_bytes(RECV_BUF, awaited.bytes as usize);
        check!(got == reply, "reply bytes changed on the way back");
        check!(string_field(&got)? == "pong", "reply payload changed");

        // 5. One-way send and receive: same bytes, no transaction id.
        let note = parcel(11, flags::ONE_WAY, "note")?;
        write_bytes(REQUEST, &note);
        let args = MsgArgs {
            handle: client,
            parcel_ptr: REQUEST,
            parcel_len: note.len() as u64,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_SEND, &args);
        check!(code == 0, "send -> {code:#x}");
        let args = MsgArgs {
            handle: server,
            buf_ptr: RECV_BUF,
            buf_cap: 4096,
            ..MsgArgs::default()
        };
        let (code, received) = syscall(OP_RECV, &args);
        check!(
            code == 0 && received.value == 0,
            "one-way recv -> {code:#x}"
        );
        check!(
            read_bytes(RECV_BUF, received.bytes as usize) == note,
            "one-way bytes changed in flight"
        );

        // 6. Counters agree with one call and one reply.
        let args = MsgArgs {
            buf_ptr: STATS_BUF,
            buf_cap: MsgStats::SIZE as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_STATS, &args);
        check!(
            code == 0 && result.bytes as usize == MsgStats::SIZE,
            "stats -> {code:#x}"
        );
        let stats = MsgStats::from_bytes(&read_bytes(STATS_BUF, MsgStats::SIZE))
            .ok_or("bad stats block")?;
        check!(
            stats.calls == 1 && stats.replies == 1 && stats.outstanding == 0,
            "counters after one echo: {stats:?}"
        );
        check!(stats.queued == 0, "channel not drained: {stats:?}");

        serial_println!("MSG:ECHO:PASS");

        // 7. Cancel a registered call; the await reports the cancellation.
        let stuck = parcel(12, flags::SYNC, "stuck")?;
        write_bytes(REQUEST, &stuck);
        let args = MsgArgs {
            handle: client,
            parcel_ptr: REQUEST,
            parcel_len: stuck.len() as u64,
            ..MsgArgs::default()
        };
        let (code, begun) = syscall(OP_CALL_BEGIN, &args);
        check!(code == 0, "begin(stuck) -> {code:#x}");
        let args = MsgArgs {
            txn_id: begun.value,
            ..MsgArgs::default()
        };
        check!(syscall(OP_CANCEL, &args).0 == 0, "cancel failed");
        let args = MsgArgs {
            txn_id: begun.value,
            buf_ptr: RECV_BUF,
            buf_cap: 4096,
            ..MsgArgs::default()
        };
        check!(
            syscall(OP_CALL_AWAIT, &args).0 == failed(errno::ECANCELED),
            "await after cancel was not -ECANCELED"
        );

        // 8. Closing both ends frees the handles.
        let args = MsgArgs {
            handle: client,
            ..MsgArgs::default()
        };
        check!(
            syscall(OP_CLOSE_ENDPOINT, &args).0 == 0,
            "close(client) failed"
        );
        let args = MsgArgs {
            handle: server,
            ..MsgArgs::default()
        };
        check!(
            syscall(OP_CLOSE_ENDPOINT, &args).0 == 0,
            "close(server) failed"
        );
        Ok(())
    })
}

/// The blocking `call` op parks through the timer gate and maps the
/// channel's timeout to `-ETIMEDOUT`.
pub fn syscall_timeout() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let request = parcel(7, flags::SYNC, "nobody home")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            handle: created.value,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            buf_ptr: RECV_BUF,
            buf_cap: 4096,
            deadline: task::ticks() + 1,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_CALL, &args);
        check!(
            code == failed(errno::ETIMEDOUT),
            "call -> {code:#x}, expected -ETIMEDOUT"
        );
        check!(
            result.status == -errno::ETIMEDOUT,
            "timeout status is {}",
            result.status
        );
        check!(
            channels::stats().timeouts == 1,
            "the timeout was not counted: {:?}",
            channels::stats()
        );
        Ok(())
    })
}

/// A parcel-bearing op goes through the ACL hook: with a non-empty policy
/// that does not cover the caller, the call is `-EACCES`, the channel is
/// untouched, and the denial is audited.
pub fn syscall_denied() -> Result<(), String> {
    fresh()?;
    acl::load(&[acl::Rule {
        actor: 2000,
        interface_id: IFACE,
        method: 7,
        allow: true,
    }]);
    credentials::set(
        task::KERNEL_TASK,
        credentials::Cred::new(1000, 100, 0, 0, 0),
    );
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let request = parcel(7, flags::SYNC, "blocked")?;
        write_bytes(REQUEST, &request);
        let before = audit::count();
        let args = MsgArgs {
            handle: created.value,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, result) = syscall(OP_CALL_BEGIN, &args);
        check!(
            code == failed(errno::EACCES),
            "denied call_begin -> {code:#x}"
        );
        check!(
            result.status == -errno::EACCES,
            "denied status is {}",
            result.status
        );
        let stats = channels::stats();
        check!(
            stats.calls == 0 && stats.queued == 0,
            "a denied call touched the channel: {stats:?}"
        );
        check!(
            audit::count() == before + 1,
            "the denial was not audited: {} -> {}",
            before,
            audit::count()
        );
        let event = *audit::recent(1).first().ok_or("no audit event")?;
        check!(
            !event.allow && event.interface_id == IFACE && event.method == 7,
            "the denial event is {event:?}"
        );
        check!(
            event.reason_code == acl::reason::DEFAULT_DENY,
            "the denial reason code is {}",
            event.reason_code
        );
        Ok(())
    })
}
