//! `OP_RECV` and a message the receiver cannot take (audit H2): an
//! oversized parcel is dropped with `EMSGSIZE` and its caller is told, a bad
//! buffer costs the receiver nothing, and the endpoint serves on.

use super::*;

/// Bytes of the oversized request: more than the receive buffer offered
/// below, fewer than the wire limit.
const BIG: usize = 5000;
const SMALL_BUFFER: u64 = 4096;

pub fn syscall_recv_drops_oversized() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let (client, server) = (created.value, created.aux);

        // An oversized call: the receiver sees EMSGSIZE, not E2BIG, and its
        // caller learns at once (EPIPE) instead of waiting for a reply that
        // can never come.
        let big = parcel(7, flags::SYNC, &"x".repeat(BIG))?;
        write_bytes(REQUEST, &big);
        let args = MsgArgs {
            handle: client,
            parcel_ptr: REQUEST,
            parcel_len: big.len() as u64,
            ..MsgArgs::default()
        };
        let (code, begun) = syscall(OP_CALL_BEGIN, &args);
        check!(code == 0, "call_begin -> {code:#x}");
        let doomed = begun.value;
        let recv = MsgArgs {
            handle: server,
            buf_ptr: RECV_BUF,
            buf_cap: SMALL_BUFFER,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_RECV, &recv);
        check!(
            code == failed(errno::EMSGSIZE),
            "oversized recv -> {code:#x}"
        );
        let args = MsgArgs {
            txn_id: doomed,
            buf_ptr: REPLY_BUF,
            buf_cap: SMALL_BUFFER,
            ..MsgArgs::default()
        };
        let (code, _) = syscall(OP_CALL_AWAIT, &args);
        check!(
            code == failed(errno::EPIPE),
            "the caller of a dropped request got {code:#x}"
        );
        check!(
            channels::stats().queued == 0 && channels::stats().outstanding == 0,
            "the drop left {:?}",
            channels::stats()
        );

        // A buffer the kernel cannot write is refused *before* the message
        // is taken: it is still queued for the next, valid receive.
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
        let bad = MsgArgs {
            buf_ptr: 0xdead_0000,
            ..recv
        };
        let (code, _) = syscall(OP_RECV, &bad);
        check!(
            code == failed(errno::EFAULT),
            "recv into nowhere -> {code:#x}"
        );
        check!(
            channels::stats().queued == 1,
            "a refused receive took the message"
        );
        let (code, received) = syscall(OP_RECV, &recv);
        check!(code == 0, "recv after the refusal -> {code:#x}");
        check!(
            read_bytes(RECV_BUF, received.bytes as usize) == note,
            "the note changed in flight"
        );
        Ok(())
    })
}
