//! `powerctl` (`/system/bin/powerctl`): ask `init` for an orderly shutdown or reboot
//! (docs/shutdown.md). The shell's `shutdown`, `poweroff` and `halt` run it as
//! `powerctl poweroff`, and `reboot` as `powerctl reboot`.
//!
//! ```text
//! powerctl poweroff [-f] [reason...]   stop the services, sync, power off
//! powerctl reboot   [-f] [reason...]   the same, then reset the machine
//! ```
//!
//! `-f` asks `init` to skip the graceful stop (it still syncs). The command
//! returns as soon as `init` accepted: the stop itself happens after, and the
//! shell running this is one of the apps it ends.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use user::messenger::services::{self, INIT_NAME, POWER_MODE_POWER_OFF, POWER_MODE_REBOOT};
use user::messenger::Error;
use user::sys;

/// How long to wait for `init`'s answer (100 Hz): it replies before stopping
/// anything, so this is a backstop.
const REPLY_TICKS: u64 = 500;

const USAGE: &str = "usage: powerctl poweroff|reboot [-f] [reason...]\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let mut buffer = [0u8; 256];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = String::from(core::str::from_utf8(&buffer[..len]).unwrap_or(""));
    let status = match run(&text) {
        Ok(()) => 0,
        Err(message) => {
            sys::write_str(&message);
            1
        }
    };
    sys::exit(status)
}

fn run(text: &str) -> Result<(), String> {
    let mut words = text.split_whitespace();
    let (mode, name) = match words.next() {
        Some("poweroff") => (POWER_MODE_POWER_OFF, "power-off"),
        Some("reboot") => (POWER_MODE_REBOOT, "reboot"),
        _ => return Err(String::from(USAGE)),
    };
    let mut force = false;
    let mut reason: Vec<&str> = Vec::new();
    for word in words {
        match word {
            "-f" | "--force" => force = true,
            _ => reason.push(word),
        }
    }
    let reason = if reason.is_empty() {
        format!("{name} requested from the shell")
    } else {
        reason.join(" ")
    };
    let init = services::resolve_service(INIT_NAME).map_err(|error| failure(&error))?;
    let deadline = Some(sys::clock() + REPLY_TICKS);
    let phase = services::shutdown(&init, mode, &reason, force, deadline)
        .map_err(|error| failure(&error))?;
    sys::write_str(&format!("powerctl: {name} accepted (phase {phase})\n"));
    Ok(())
}

/// The message for a refused or failed request.
fn failure(error: &Error) -> String {
    let detail = match error.errno() {
        Some(code) if code == -user::messenger::errno::EPERM => "not permitted",
        Some(code) if code == -user::messenger::errno::EINVAL => "invalid request",
        _ => error.message(),
    };
    format!("powerctl: {detail}\n")
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
