//! `dragdemo` (`/system/bin/dragdemo`): the compositor-mediated drag & drop demo pair
//! (issue #145).
//!
//! The kernel boots this program next to `xuid` with no manifest argument, so
//! it is the **launcher**: it starts a `source` and a `target` child of itself
//! (two separate display clients in one clipboard session) and reaps them.
//!
//! * the **source** offers a typed payload through `clipboardd`
//!   (`text/x-dnd-demo`), draws a coloured item, and when a press travels past
//!   [`DRAG_THRESHOLD`] calls `DragStart(surface, token, mime)`; the
//!   compositor owns the pointer from there;
//! * the **target** receives `DragEnter`/`DragLeave`/`Drop` from the
//!   compositor and, on a drop, pastes the delivered token back through
//!   `clipboardd` — the same authorization path as any paste — then checks the
//!   bytes and shows their type and length.
//!
//! The cross-session denial probe runs in a short-lived `probe` child of the
//! target, so switching into the probe session never changes the target's own
//! credentials: every later drop in the same boot still pastes normally.
//!
//! Evidence markers: `DND:START:PASS` (the compositor accepted the drag),
//! `DND:DROP:PASS` (the target pasted the token), `DND:CANCEL:PASS` (Escape
//! cancelled the drag) and `DND:DENIED:PASS` (a cross-session paste of the
//! dropped token is refused). Failures print `DND:<name>:FAIL:<detail>`.
//!
//! The roles live in sibling modules (issue #194 split): [`app`] is the shared
//! display-client plumbing, [`launcher`] starts and reaps the two clients,
//! [`source`] offers and drags the payload, [`target`] receives drops and
//! [`probe`] drives the cross-session denial check.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "dragdemo/app.rs"]
mod app;
#[path = "dragdemo/launcher.rs"]
mod launcher;
#[path = "dragdemo/probe.rs"]
mod probe;
#[path = "dragdemo/source.rs"]
mod source;
#[path = "dragdemo/target.rs"]
mod target;

use alloc::string::String;
use core::panic::PanicInfo;
use user::messenger::display::{self, Rect};
use user::sys;

use launcher::launcher;
use probe::probe;
use source::source;
use target::target;

/// Surface content size in pixels.
const W: i32 = 220;
const H: i32 = 180;
/// MIME type the source offers and the target expects.
const DEMO_MIME: &str = "text/x-dnd-demo";
/// The payload; the target checks the bytes it pastes against this.
const PAYLOAD: &[u8] = b"dnd payload #42";
/// Pointer travel (surface pixels) before a press becomes a drag.
const DRAG_THRESHOLD: i64 = 6;
/// PIT ticks the target waits for its denial-probe child to exit.
const PROBE_TICKS: u64 = 2000;
/// PIT ticks `connect` retries while the compositor/services settle.
const CONNECT_ATTEMPTS: usize = 200;
/// The session the denial probe switches to (mirrors `clippaste`).
const PROBE_SESSION: u64 = 4242;
/// The drag source's item rectangle inside its content.
const ITEM: Rect = Rect::new(24, 52, 56, 56);

#[no_mangle]
pub extern "C" fn _start() -> ! {
    match role() {
        Role::Launcher => launcher(),
        Role::Source => source(),
        Role::Target => target(),
        Role::Probe { token, mime } => probe(token, &mime),
    }
}

/// The role the launcher passed in the kernel's service argument string.
enum Role {
    Launcher,
    Source,
    Target,
    /// `probe <token> <mime>`: the target's one-shot denial-probe child.
    Probe {
        token: u64,
        mime: String,
    },
}

/// Read this task's manifest argument (`""` for a kernel-spawned launcher,
/// `source`/`target` for the children, `probe <token> <mime>` for the
/// target's denial-probe child).
fn role() -> Role {
    let mut buffer = [0u8; 32 + display::MAX_MIME];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let args = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    let mut words = args.split(' ');
    match words.next().unwrap_or("") {
        "source" => Role::Source,
        "target" => Role::Target,
        "probe" => {
            let token = words.next().and_then(|word| word.parse().ok()).unwrap_or(0);
            let mime = String::from(words.next().unwrap_or(""));
            Role::Probe { token, mime }
        }
        _ => Role::Launcher,
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::write_str("dragdemo: panic\n");
    sys::exit(1)
}
