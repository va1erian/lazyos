//! `recv`'s `RECV_SENDER_ID` flag: the sender's identity as the kernel
//! stamped it when the message was queued, delivered to a receiver that holds
//! no capability (LazyShell's `os.lazy.shell.v1` authorizes callers by it).

use super::*;
use crate::ipc::channels::SenderId;
use crate::ipc::credentials::Cred;
use crate::ipc::syscalls::RECV_SENDER_ID;
use crate::quota::{self, Resource};

/// `-EINVAL` as `rax` carries it.
const EINVAL_CODE: u64 = (-22i64) as u64;
/// `-EFAULT` as `rax` carries it.
const EFAULT_CODE: u64 = (-14i64) as u64;

/// Where the tests ask `recv` to write the sender block.
const SENDER_BUF: u64 = SPACE + 0x5000;

/// A pattern `recv` must overwrite (or, without the flag, leave alone).
const UNTOUCHED: [u8; SenderId::SIZE] = [0xa5; SenderId::SIZE];

/// The sender block the kernel writes for `cred`: no capability bits.
fn expected(cred: Cred) -> [u8; SenderId::SIZE] {
    let mut bytes = [0u8; SenderId::SIZE];
    let words = [
        cred.uid as u64,
        cred.gid as u64,
        cred.label_id as u64,
        cred.session,
    ];
    for (chunk, word) in bytes.as_chunks_mut::<8>().0.iter_mut().zip(words) {
        chunk.copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

/// Queue a one-way note from the current task through the syscall.
fn send_note(client: u64, text: &str) -> Result<(), String> {
    let note = parcel(21, flags::ONE_WAY, text)?;
    write_bytes(REQUEST, &note);
    let args = MsgArgs {
        handle: client,
        parcel_ptr: REQUEST,
        parcel_len: note.len() as u64,
        ..MsgArgs::default()
    };
    let (code, _) = syscall(OP_SEND, &args);
    check!(code == 0, "send -> {code:#x}");
    Ok(())
}

/// `recv` on `server` with `flags`, the sender block offered at
/// [`SENDER_BUF`] with `sender_len` bytes.
fn recv_with(server: u64, flags: u64, sender_len: u64) -> (u64, MsgResult) {
    write_bytes(SENDER_BUF, &UNTOUCHED);
    let args = MsgArgs {
        handle: server,
        parcel_ptr: SENDER_BUF,
        parcel_len: sender_len,
        buf_ptr: RECV_BUF,
        buf_cap: 4096,
        flags,
        ..MsgArgs::default()
    };
    syscall(OP_RECV, &args)
}

/// The block carries the identity at *queue* time, not the sender's current
/// one, and never its capabilities; a plain `recv` writes nothing there; a
/// call (not only a one-way message) carries it too.
pub fn recv_reports_queue_time_sender() -> Result<(), String> {
    fresh()?;
    let outcome = in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let (client, server) = (created.value, created.aux);

        let caller = Cred::new(1000, 100, credentials::CAP_SETUID, 0, 77);
        credentials::set(task::KERNEL_TASK, caller);
        send_note(client, "who")?;
        // The sender changes identity after queueing: the stamp must not.
        credentials::set(task::KERNEL_TASK, Cred::new(2000, 200, 0, 0, 5));
        let (code, received) = recv_with(server, RECV_SENDER_ID, SenderId::SIZE as u64);
        check!(code == 0, "recv with the flag -> {code:#x}");
        check!(
            read_bytes(RECV_BUF, received.bytes as usize) == parcel(21, flags::ONE_WAY, "who")?,
            "the parcel changed when the sender block was asked for"
        );
        let block = read_bytes(SENDER_BUF, SenderId::SIZE);
        check!(
            block == expected(caller),
            "sender block {block:02x?} (expected the queue-time identity)"
        );

        // Without the flag nothing is written to `parcel_ptr`.
        send_note(client, "plain")?;
        let (code, _) = recv_with(server, 0, SenderId::SIZE as u64);
        check!(code == 0, "plain recv -> {code:#x}");
        check!(
            read_bytes(SENDER_BUF, SenderId::SIZE) == UNTOUCHED,
            "a plain recv wrote the sender block"
        );

        // A synchronous call carries the stamp too.
        let now = Cred::new(3000, 300, 0, 0, 9);
        credentials::set(task::KERNEL_TASK, now);
        let request = parcel(22, flags::SYNC, "call")?;
        write_bytes(REQUEST, &request);
        let args = MsgArgs {
            handle: client,
            parcel_ptr: REQUEST,
            parcel_len: request.len() as u64,
            ..MsgArgs::default()
        };
        let (code, begun) = syscall(OP_CALL_BEGIN, &args);
        check!(code == 0, "call_begin -> {code:#x}");
        let (code, received) = recv_with(server, RECV_SENDER_ID, 64);
        check!(
            code == 0 && received.value == begun.value,
            "recv call -> {code:#x}"
        );
        check!(
            read_bytes(SENDER_BUF, SenderId::SIZE) == expected(now),
            "a call's sender block is wrong"
        );
        let (code, _) = syscall(
            OP_CANCEL,
            &MsgArgs {
                txn_id: begun.value,
                ..MsgArgs::default()
            },
        );
        check!(code == 0, "cancel -> {code:#x}");
        Ok(())
    });
    credentials::reset_for_task(task::KERNEL_TASK);
    outcome
}

/// A sender block too small for the identity, one that is not writable user
/// memory, or an unknown flag, is refused before anything is taken off the
/// queue: the message is still there.
pub fn recv_sender_id_refuses_bad_requests() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let (client, server) = (created.value, created.aux);
        send_note(client, "kept")?;

        for short in [0u64, 1, SenderId::SIZE as u64 - 1] {
            let (code, _) = recv_with(server, RECV_SENDER_ID, short);
            check!(code == EINVAL_CODE, "a {short}-byte block -> {code:#x}");
        }
        for bad in [2u64, 3, 1 << 32, u64::MAX] {
            let (code, _) = recv_with(server, bad, SenderId::SIZE as u64);
            check!(code == EINVAL_CODE, "flags {bad:#x} on recv -> {code:#x}");
        }
        // Unmapped user memory, a block straddling the end of the scratch
        // space, and a kernel address.
        let end = SPACE + SPACE_PAGES * 4096;
        for bad in [end + 0x1000, end - 8, 0xffff_8000_0000_0000] {
            let args = MsgArgs {
                handle: server,
                parcel_ptr: bad,
                parcel_len: SenderId::SIZE as u64,
                buf_ptr: RECV_BUF,
                buf_cap: 4096,
                flags: RECV_SENDER_ID,
                ..MsgArgs::default()
            };
            let (code, _) = syscall(OP_RECV, &args);
            check!(code == EFAULT_CODE, "sender block at {bad:#x} -> {code:#x}");
        }
        let stats = channels::channel_stats(server).map_err(reason)?;
        check!(
            stats.queued == 1,
            "a refused recv took the message: {stats:?}"
        );

        let (code, received) = recv_with(server, RECV_SENDER_ID, SenderId::SIZE as u64);
        check!(code == 0, "the valid recv -> {code:#x}");
        check!(
            string_field(&read_bytes(RECV_BUF, received.bytes as usize))? == "kept",
            "the queued message changed"
        );
        check!(
            read_bytes(SENDER_BUF, SenderId::SIZE) == expected(Cred::ROOT),
            "the kernel task's block is not root"
        );
        Ok(())
    })
}

/// The kernel's own posts carry the kernel identity, and labelled senders
/// their label: checked on the channel API, below the syscall's ACL.
pub fn sender_id_carries_labels_and_the_kernel() -> Result<(), String> {
    fresh()?;
    let outcome = (|| -> Result<(), String> {
        let (client, server) = channels::create().map_err(reason)?;
        let labelled = Cred::new(1000, 100, 0, 7, 3);
        credentials::set(task::KERNEL_TASK, labelled);
        channels::send(client, &parcel(23, flags::ONE_WAY, "app")?).map_err(reason)?;
        credentials::reset_for_task(task::KERNEL_TASK);
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("no message")?;
        check!(
            message.origin == SenderId::of_cred(labelled),
            "labelled origin {:?}",
            message.origin
        );

        let object = handles::get(server)
            .map_err(|error| format!("{error:?}"))?
            .object_id;
        let (channel_id, side) = (object >> 1, (object & 1) as usize);
        channels::post_from_kernel(channel_id, side, &parcel(24, flags::ONE_WAY, "k")?)
            .map_err(reason)?;
        let message = channels::try_recv(server)
            .map_err(reason)?
            .ok_or("no kernel post")?;
        check!(
            message.origin == SenderId::KERNEL,
            "kernel origin {:?}",
            message.origin
        );
        Ok(())
    })();
    credentials::reset_for_task(task::KERNEL_TASK);
    outcome
}

/// Soak: thousands of messages from a sender whose identity changes between
/// every send, received in bursts; each block matches its own queue-time
/// identity, and every per-uid queue charge is returned to the uid that paid.
pub fn sender_id_soak() -> Result<(), String> {
    const ROUNDS: u32 = 400;
    const BURST: u32 = 8;
    fresh()?;
    let outcome = in_space(|| -> Result<(), String> {
        let (code, created) = syscall(OP_CREATE_PAIR, &MsgArgs::default());
        check!(code == 0, "create_pair -> {code:#x}");
        let (client, server) = (created.value, created.aux);
        let identity = |n: u32| Cred::new(1000 + n % 5, 100 + n % 3, 0, 0, n as u64);
        for round in 0..ROUNDS {
            for index in 0..BURST {
                credentials::set(task::KERNEL_TASK, identity(round * BURST + index));
                send_note(client, "soak")?;
            }
            credentials::set(task::KERNEL_TASK, Cred::new(4242, 0, 0, 0, 0));
            for index in 0..BURST {
                let (code, _) = recv_with(server, RECV_SENDER_ID, SenderId::SIZE as u64);
                check!(code == 0, "round {round} recv {index} -> {code:#x}");
                let want = expected(identity(round * BURST + index));
                check!(
                    read_bytes(SENDER_BUF, SenderId::SIZE) == want,
                    "round {round} message {index}: wrong sender block"
                );
            }
        }
        let stats = channels::channel_stats(server).map_err(reason)?;
        check!(stats.queued == 0, "soak left messages queued: {stats:?}");
        for uid in [1000, 1001, 1002, 1003, 1004, 4242] {
            let left = quota::usage(uid, Resource::QueueBytes);
            check!(left == 0, "uid {uid} still charged {left} queued bytes");
        }
        Ok(())
    });
    credentials::reset_for_task(task::KERNEL_TASK);
    outcome
}
