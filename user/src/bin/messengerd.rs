//! `messengerd` (`MSGRD.ELF`): the bootstrap registry daemon (issue #89) and
//! the topics broker (issue #92).
//!
//! This is the userspace half of `docs/messenger.md` section 8. The kernel
//! owns the name table (`ipc::registry`) and publishes the bootstrap listener
//! as `os.lazy.messenger.registry`; this program claims the other end of the
//! bootstrap channel and serves requests for the life of the system.
//!
//! For names the daemon is deliberately thin: [`registry::serve_request`]
//! forwards each request to the kernel with the *requester's* task slot, so
//! the kernel opens a resolved handle straight into the requester's table and
//! records the requester as the owner of a registration. Nothing but the
//! request body and the kernel-stamped sender slot crosses this process;
//! handle numbers never have to be translated here.
//!
//! For topics the daemon is the broker. Section 20's epic decision puts
//! pub/sub in userspace first, and this file is that decision taken
//! literally: [`Broker`] owns hierarchical names, `+`/`#` filter matching,
//! QoS queues, retained values and per-subscriber drop counters, while every
//! publish and subscribe still asks the kernel's policy engine
//! (`topics_client::authorize`) before a byte is stored. Delivery is pull-based with
//! deferred replies: `NextEvent` is answered at once when an event is
//! queued, or parked (the kernel keeps the caller asleep with a real
//! deadline) until a matching publish arrives. That keeps the single-threaded
//! daemon non-blocking and the publishers free of subscriber stalls.
//!
//! The on-disk name is `MSGRD.ELF`: 8.3-safe, because the kernel's FAT
//! reader only resolves short names.
//!
//! Boot it with `LAZYOS_MESSENGERD=1` (see the kernel build script); the demo
//! then starts this program alongside `hello` and `sh`.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "messengerd/broker.rs"]
mod broker;
#[path = "messengerd/filter.rs"]
mod filter;
#[path = "messengerd/handlers.rs"]
mod handlers;
#[path = "messengerd/soak.rs"]
mod soak;

use alloc::format;
use core::panic::PanicInfo;
use user::messenger::{self, errno, registry, topics_client};
use user::sys;

use broker::Broker;
use handlers::serve_topic;
use soak::{await_reply_with, finish_soak, soak_cycles, Soak, SOAK_IDLE_TICKS};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("messengerd: starting (registry daemon #89, topics broker #92)\n");
    if let Err(error) = serve() {
        sys::write_str("messengerd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Claim the bootstrap channel, register the topics service, and serve
/// registry and topic requests forever.
fn serve() -> messenger::Result<()> {
    // The kernel holds the service end and keeps it published under the
    // well-known name; claiming the client end is what makes this task the
    // listener those resolved calls arrive at.
    let endpoint = messenger::bootstrap()?;
    sys::write_str("messengerd: bootstrap endpoint claimed\n");

    // Exercise the direct registry API once so the boot log proves the table
    // is reachable from userspace (the kernel has published one name by now).
    match registry::list() {
        Ok(entries) => sys::write_str(&format!(
            "messengerd: registry ready, {} name(s)\n",
            entries.len()
        )),
        Err(error) => {
            sys::write_str("messengerd: registry list failed: ");
            sys::write_str(error.message());
            sys::write_str("\n");
        }
    }

    // Publish the topics service name next to the registry one. Clients
    // resolve it to find the broker; `Client::connect` retries while this
    // registration is still in flight. Resolving the registry name gives this
    // task a handle to the *service* side of the bootstrap channel, which is
    // the object the new name must refer to (the same one the kernel
    // published).
    let service = registry::resolve(registry::NAME)?;
    registry::register(
        topics_client::NAME,
        &service,
        &[topics_client::INTERFACE],
        0,
    )?;
    sys::write_str("messengerd: topics service registered as ");
    sys::write_str(topics_client::NAME);
    sys::write_str("\n");

    let mut broker = Broker::new();
    // Self-soak mode (`soak=N` from the supervisor's manifest): drive N
    // request/reply cycles through this very loop and assert the daemon's bump
    // heap did not grow across them (issue #169). Off unless asked for, so a
    // plain boot serves at full speed.
    let mut soak = soak_cycles().map(Soak::start);
    if let Some(soak) = &soak {
        sys::write_str(&format!("MSGRD:SOAK:START cycles={}\n", soak.cycles_total));
    }
    // One receive buffer for the life of the daemon. The user runtime's bump
    // allocator never reclaims per-call buffers, so `Endpoint::recv`'s fresh
    // `DEFAULT_BUFFER` per message would OOM the broker after a few hundred
    // calls; `recv_with` reuses this one instead.
    let mut recv_buffer = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    sys::write_str("messengerd: serving\n");

    loop {
        // Queue the next self-soak call. It is an ordinary call on the
        // bootstrap channel, answered by the dispatch below, so the soak
        // exercises the same path a client's poll does.
        if let Some(soak) = soak.as_mut() {
            if soak.cycles_left > 0 && soak.txn.is_none() {
                match service.begin_call(&soak.request, None) {
                    Ok(txn) => soak.txn = Some(txn),
                    Err(_) => soak.fail(),
                }
            }
        }
        let deadline = match soak.as_ref() {
            Some(soak) if soak.cycles_left == 0 => Some(sys::clock() + SOAK_IDLE_TICKS),
            _ => None,
        };
        let message = match endpoint.recv_with(&mut recv_buffer, deadline) {
            Ok(message) => message,
            // The idle wait after the soak's cycles: no message arrived, so
            // re-check whether the soak can report. Every other timeout is a
            // failure.
            Err(messenger::Error::Errno(code)) if code == -errno::ETIMEDOUT && soak.is_some() => {
                if soak.as_ref().is_some_and(|soak| soak.done(&broker)) {
                    let soak = soak.take().expect("checked above");
                    finish_soak(&broker, &soak);
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        if message.interface_id() == topics_client::INTERFACE {
            serve_topic(&endpoint, &mut broker, &message);
        } else {
            let reply = match registry::serve_request(&message.parcel, message.sender) {
                Ok(reply) => reply,
                // A failed request still gets an answer, or the caller would
                // wait forever. The error reply carries the code and the
                // friendly text.
                Err(error) => registry::error_reply(message.method(), error),
            };
            // A reply can fail because the caller timed out and its
            // transaction is gone; that is a normal race, not a fatal error.
            if let Some(txn) = message.txn {
                if endpoint.reply(txn, &reply).is_err() {
                    sys::write_str("messengerd: registry reply dropped (caller gone)\n");
                }
            }
        }
        // Consume a self-soak reply with a reused buffer: `await_reply` would
        // allocate a fresh 16 KiB one per cycle, which is exactly what this
        // soak exists to keep off the serve path. The reply is already queued.
        if let Some(soak) = soak.as_mut() {
            if message.txn.is_some() && message.txn == soak.txn {
                let txn = soak.txn.take().expect("checked above");
                if await_reply_with(txn, &mut soak.reply_buffer).is_err() {
                    soak.fail();
                }
                soak.cycles_left = soak.cycles_left.saturating_sub(1);
            }
        }
        if soak.as_ref().is_some_and(|soak| soak.done(&broker)) {
            let soak = soak.take().expect("checked above");
            finish_soak(&broker, &soak);
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
