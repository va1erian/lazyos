//! `audiod` (`AUDIOD.ELF`): the system mixer (docs/audio-plan.md stage A3).
//!
//! Applications never touch the sound card. They resolve `os.lazy.audio` and
//! speak `os.lazy.audio.v1` to this service, exactly as they would to a card:
//! open a stream, share a ring, commit, start, drain, close. Any number of
//! clients may do so at once. Every tick the mixer reads what each running
//! stream committed, converts it to the card's rate, scales it by the
//! stream's volume, sums the streams, applies the master volume and queues
//! the period on the card, whose one stream (`sndd`, `os.lazy.audio.card`) it
//! holds for as long as it runs. The control panel, `os.lazy.audio.mixer.v1`,
//! lists the streams and sets any stream's volume and the master volume.
//!
//! The engine and the wire rules are `libs/audiomix` (host-tested); this
//! program adds Messenger, the card and the mapped client rings. Under
//! `init` it runs as `_audio` (uid 905) with no capabilities at all.
//!
//! `demo=1` runs the sound harness's evidence clients through the mixer once a
//! card is attached (`audiod/demo.rs`).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use core::panic::PanicInfo;

use user::messenger::audio as api;
use user::messenger::{self, errno, registry, Error as MsgError};
use user::sys;

#[path = "audiod/card.rs"]
mod card;
#[path = "audiod/demo.rs"]
mod demo;
#[path = "audiod/ring.rs"]
mod ring;
#[path = "audiod/server.rs"]
mod server;

use demo::Demo;
use server::Server;

/// Longest park while nothing plays (PIT ticks); the card keepalive and the
/// reclaim timer are far coarser.
const IDLE_TICKS: u64 = 100;
/// Park while evidence clients run, so the next one starts promptly.
const DEMO_TICKS: u64 = 10;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("audiod: system mixer\n");
    let mut cred = sys::Cred::default();
    if sys::cred_get(None, &mut cred).is_ok() {
        sys::write_str(&format!(
            "AUDIOD:CRED uid={} caps={:#x}\n",
            cred.uid, cred.caps
        ));
    }
    match run(demo_requested()) {
        Ok(()) => sys::exit(0),
        Err(error) => {
            sys::write_str(&format!("AUDIOD:FAIL {}\n", error.message()));
            sys::exit(1)
        }
    }
}

/// Whether the service arguments ask for the evidence run (`demo=1`).
fn demo_requested() -> bool {
    let mut buffer = [0u8; 64];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().any(|part| part == "demo=1")
}

/// Register both interfaces under `os.lazy.audio` and serve them for the life
/// of the mixer.
fn run(demo: bool) -> Result<(), MsgError> {
    let (published, endpoint) = messenger::create_pair()?;
    registry::register(
        api::NAME,
        &published,
        &[api::INTERFACE, api::CONTROL_INTERFACE],
        0,
    )?;
    sys::write_str(&format!("AUDIOD:READY name={}\n", api::NAME));

    let mut server = Server::new();
    let mut demo = Demo::new(demo);
    // One receive buffer for the life of the service (the heap never reclaims
    // per-call buffers).
    let mut buffer = vec![0u8; messenger::DEFAULT_BUFFER];
    let mut stepped_at = None;
    loop {
        // Card pacing, drains and reclaims run at most once per tick, however
        // many requests arrive.
        let now = sys::clock();
        if stepped_at != Some(now) {
            stepped_at = Some(now);
            server.step(&endpoint, now);
            demo.poll(server.has_card());
        }
        let park = if server.busy() {
            1
        } else if demo.active() {
            DEMO_TICKS
        } else {
            IDLE_TICKS
        };
        match endpoint.recv_with(&mut buffer, Some(sys::clock() + park)) {
            Ok(message) => {
                if let (Some(reply), Some(txn)) = (server.dispatch(&message), message.txn) {
                    endpoint.reply_or_drop(txn, &reply)?;
                }
            }
            Err(MsgError::Errno(code)) if code == -errno::ETIMEDOUT => {}
            Err(error) => return Err(error),
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
