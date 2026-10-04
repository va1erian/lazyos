//! `clipboardd` (`/system/bin/clipboardd`): the per-session clipboard service (issue #115).
//!
//! This is the S4 clipboard from `docs/platform-plan.md` section 4.5 and the
//! worked example in `docs/messenger.md` section 19:
//!
//! * a client offers typed payloads (MIME strings) for **its session** and
//!   receives a token (`Offer(owner, mime_types) -> token`);
//! * a paster asks for a payload (`Request(token, mime)`); the service answers
//!   with a [`wire::BufferHandle`]. An **eager** offer keeps a bounded copy;
//!   a **lazy** offer names a registry endpoint (`sink`) and the service calls
//!   the owner's `Serialize` only when a paste happens, so the owner
//!   materializes the data on demand;
//! * every offer is announced, retained, on
//!   `session/<id>/clipboard/changed`, so paste UIs refresh without polling;
//! * the history policy is one offer per session by default; the supervisor
//!   can pass `history=N` in the manifest argument string to keep up to
//!   [`MAX_HISTORY`].
//!
//! # Policy
//!
//! `Offer` and `Request` parcels carry the `clipboard.write` / `clipboard.read`
//! pseudo-interface ids in their header, so the kernel's `ipc::authorize` hook
//! (and its audit ring) gates them like any other Messenger call. The service
//! adds the session scope: the session id of every request comes from the
//! kernel-stamped credentials (`sys::cred_get`, which needs `CAP_SETUID` — the
//! supervisor starts this service as root today), never from the parcel. A
//! token offered by one session is refused for another; the refusal is counted,
//! printed, and published on `system/events/security/clipboard` so `logd`
//! retains it next to the kernel's own denials.
//!
//! # Buffer handle
//!
//! The kernel's `SHARE_ONLY` shared buffers (`kernel/src/ipc/shared.rs`) are
//! the future transport for pastes; there is no userspace mapping syscall yet
//! (`keyd` documents the same gap), so [`wire::BufferHandle`] carries the bytes
//! in the reply parcel and the service bounds inline eager payloads at
//! [`wire::MAX_DATA`]. The protocol does not change when the mapping op lands.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "clipboardd/args.rs"]
mod args;
#[path = "clipboardd/handlers.rs"]
mod handlers;
#[path = "clipboardd/serve.rs"]
mod serve;
#[path = "clipboardd/state.rs"]
mod state;

use core::panic::PanicInfo;
use user::sys;

/// Sessions the service keeps concurrently.
const MAX_SESSIONS: usize = 16;
/// Hard cap on `history=N`.
const MAX_HISTORY: usize = 8;
/// Default history: one current offer per session.
const DEFAULT_HISTORY: usize = 1;
/// PIT ticks the service waits for an owner's `Serialize` answer.
const SERIALIZE_DEADLINE: u64 = 100;
/// The evidence programs `demo=1` spawns at startup and reaps.
const DEMO_PROGRAMS: [&str; 2] = [fhs::bin::CLIPCP, fhs::bin::CLIPPASTE];

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("clipboardd: per-session clipboard service (issue #115)\n");
    if let Err(error) = serve::run() {
        sys::write_str("clipboardd: fatal: ");
        sys::write_str(error.message());
        sys::write_str("\n");
        sys::exit(1);
    }
    sys::exit(0)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
