//! The start menu's power rows, acted on (docs/shutdown.md): a confirmed
//! "Restart now" / "Shut down now" asks `init` for an orderly stop.
//!
//! The shell raises nothing itself: `init` publishes its first
//! `system/power/state` phase before stopping anything, and the compositor
//! paints the shutting-down overlay from that retained topic. `init` then
//! stops the session apps, LazyShell included, and does not restart it.
//!
//! Who may ask: `init` admits root or a caller in a login session, and never
//! a labelled (installed) app. The desktop image's LazyShell is autostarted
//! by `init` as uid 0 and passes; a shell launched into a user's session runs
//! with that session's id and passes too. A refusal is reported, and the
//! desktop carries on.
//!
//! Serial markers: `SHELL:POWER:CONFIRM`, `SHELL:POWER:REQUEST mode=<m>
//! phase=<p>`, `SHELL:POWER:REQUEST:FAIL mode=<m> errno=<e>` (`m` is the
//! `PowerMode` value: 0 power off, 1 reboot).

use lazyshell::menu::Power;
use messenger_generated::os_lazy_init_v1 as init_wire;

use super::services;

/// The reason `init` logs and publishes.
const REASON: &str = "requested from the start menu";

/// The confirmation rows are showing.
pub fn confirming() {
    println!("SHELL:POWER:CONFIRM");
}

/// Ask `init` to stop the machine with `power`.
pub fn request(power: Power) {
    let mode = match power {
        Power::PowerOff => init_wire::POWER_MODE_POWER_OFF,
        Power::Reboot => init_wire::POWER_MODE_REBOOT,
    };
    match services::shutdown(mode, REASON) {
        Ok(phase) => println!("SHELL:POWER:REQUEST mode={mode} phase={phase}"),
        Err(code) => println!("SHELL:POWER:REQUEST:FAIL mode={mode} errno={}", -code),
    }
}
