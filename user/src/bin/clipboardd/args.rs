//! Manifest-argument parsing (`history=N`, `demo=1`) and the startup demo
//! programs this service spawns and reaps itself.

use alloc::format;
use user::sys;

use super::{DEFAULT_HISTORY, DEMO_PROGRAMS, MAX_HISTORY};

/// The service's manifest-argument history depth (`history=N`, clamped).
pub(super) fn history_from_args() -> usize {
    for part in sys::args().skip(1) {
        if let Some(value) = part.strip_prefix("history=") {
            if let Ok(depth) = value.parse::<usize>() {
                return depth.clamp(1, MAX_HISTORY);
            }
        }
    }
    DEFAULT_HISTORY
}

/// Whether the manifest asked for the demo pair (`demo=1`).
pub(super) fn demo_from_args() -> bool {
    sys::args().skip(1).any(|arg| arg == "demo=1")
}

/// Spawn the two demo clients as children of this service; returns how many
/// started. They are evidence programs, not supervised services, so the
/// service reaps them itself.
pub(super) fn spawn_demo() -> u64 {
    let mut started = 0u64;
    for program in DEMO_PROGRAMS {
        match sys::spawn_native(program, &[]) {
            Some(pid) => {
                started += 1;
                sys::write_str(&format!("clipboardd: started demo {program} (pid {pid})\n"));
            }
            None => sys::write_str(&format!("clipboardd: demo {program} spawn failed\n")),
        }
    }
    started
}
