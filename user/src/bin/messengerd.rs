//! `messengerd` (`MESSENGERD.ELF`): the bootstrap registry daemon (issue #89).
//!
//! This is the userspace half of `docs/messenger.md` section 8. The kernel
//! owns the name table (`ipc::registry`) and publishes the bootstrap listener
//! as `os.lazy.messenger.registry`; this program claims the other end of the
//! bootstrap channel and serves requests for the life of the system.
//!
//! The daemon is deliberately thin: [`registry::serve_request`] forwards each
//! request to the kernel with the *requester's* task slot, so the kernel opens
//! a resolved handle straight into the requester's table and records the
//! requester as the owner of a registration. Nothing but the request body and
//! the kernel-stamped sender slot crosses this process; handle numbers never
//! have to be translated here.
//!
//! The on-disk name is `MESSENGERD.ELF`: 8.3-safe, because the kernel's FAT
//! reader only resolves short names.
//!
//! Boot it with `LAZYOS_MESSENGERD=1` (see the kernel build script); the demo
//! then starts this program alongside `hello` and `sh`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;
use user::messenger::{self, registry};
use user::sys;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("messengerd: starting (registry daemon, issue #89)\n");
    if let Err(error) = serve() {
        sys::write_str("messengerd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

/// Claim the bootstrap channel and serve registry requests forever.
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
    sys::write_str("messengerd: serving\n");

    loop {
        let message = endpoint.recv(None)?;
        let reply = match registry::serve_request(&message.parcel, message.sender) {
            Ok(reply) => reply,
            // A failed request still gets an answer, or the caller would wait
            // forever. The error reply carries the code and the friendly text.
            Err(error) => registry::error_reply(message.method(), error),
        };
        if let Some(txn) = message.txn {
            endpoint.reply(txn, &reply)?;
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
