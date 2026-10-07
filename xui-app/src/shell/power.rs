//! The start menu's session rows, acted on (docs/shutdown.md): a confirmed
//! "Restart now" / "Shut down now" asks `init` for an orderly stop, and a
//! confirmed "Log out now" asks `logind` to end the session (issue #623).
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
//! `PowerMode` value: 0 power off, 1 reboot); for a logout
//! `SHELL:LOGOUT:REQUEST session=<id>` or `SHELL:LOGOUT:FAIL errno=<e>`.

use lazyshell::menu::Power;
use messenger_generated::os_lazy_init_v1 as init_wire;

use super::services;

/// The reason `init` logs and publishes.
const REASON: &str = "requested from the start menu";

/// The confirmation rows are showing.
pub fn confirming() {
    println!("SHELL:POWER:CONFIRM");
}

/// End the session (`logind`) or stop the machine (`init`) for `power`.
pub fn request(power: Power) {
    let mode = match power {
        Power::Logout => return logout(),
        Power::PowerOff => init_wire::POWER_MODE_POWER_OFF,
        Power::Reboot => init_wire::POWER_MODE_REBOOT,
    };
    match services::shutdown(mode, REASON) {
        Ok(phase) => println!("SHELL:POWER:REQUEST mode={mode} phase={phase}"),
        Err(code) => println!("SHELL:POWER:REQUEST:FAIL mode={mode} errno={}", -code),
    }
}

/// Ask `logind` to end this desktop session: `init` then stops its tasks
/// (this shell included) and the login screen comes back.
fn logout() {
    match services::logout() {
        Ok(session) => println!("SHELL:LOGOUT:REQUEST session={session}"),
        Err(code) => println!("SHELL:LOGOUT:FAIL errno={}", -code),
    }
}
