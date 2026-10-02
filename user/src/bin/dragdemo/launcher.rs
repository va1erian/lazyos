//! The `dragdemo` launcher role (issue #194 split): start the `source` and
//! `target` display clients as children of this task and reap them, starting
//! `clipboardd` first when no supervisor already runs it.
//!
//! Split out of `dragdemo.rs`; the code is unchanged.

use alloc::format;
use user::messenger::clipboard;
use user::sys;

use super::app::park_tick;
use super::CONNECT_ATTEMPTS;

/// Start both display clients as children (so the clipboard session scope sees
/// one session) and reap them for the life of the boot.
pub(super) fn launcher() -> ! {
    sys::write_str("dragdemo: launcher (issue #145)\n");
    ensure_clipboard();
    let mut started = 0u64;
    for role in ["source", "target"] {
        match sys::spawn_native(fhs::bin::DRAGDEMO, &[role]) {
            Some(pid) => {
                started += 1;
                sys::write_str(&format!("dragdemo: started {role} (pid {pid})\n"));
            }
            None => sys::write_str(&format!("DND:LAUNCH:FAIL:{role}\n")),
        }
    }
    if started == 2 {
        sys::write_str("DND:LAUNCH:PASS\n");
    }
    loop {
        let _ = sys::wait(sys::clock() + 10);
    }
}

/// Start `clipboardd` when nothing serves the clipboard yet: the plain
/// `LAZYOS_XUID=1` boot has no supervisor, and the drag's token transfer needs
/// the service. In services mode `init` already runs it, so this is a no-op.
fn ensure_clipboard() {
    for _ in 0..CONNECT_ATTEMPTS {
        if clipboard::Client::connect().is_ok() {
            sys::write_str("dragdemo: clipboardd already present\n");
            return;
        }
        park_tick();
    }
    match sys::spawn_native(fhs::bin::CLIPBOARDD, &[]) {
        Some(pid) => sys::write_str(&format!("dragdemo: started clipboardd (pid {pid})\n")),
        None => {
            sys::write_str("DND:LAUNCH:FAIL:clipboardd did not start\n");
        }
    }
}
