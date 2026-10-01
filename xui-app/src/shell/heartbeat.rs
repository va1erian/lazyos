//! The heartbeat's schedule: when to re-read the zone, the theme and the
//! launchers, and when to retry what failed (the taskbar panel, the service
//! registration). All deadlines are PIT ticks (100 Hz).

use super::ctx::Ctx;
use super::services;
use crate::sys;

/// The zone is re-read once a minute once known (the clock itself follows the
/// kernel's UTC every heartbeat, so only a zone change waits for this).
const ZONE_TICKS: u64 = 6000;
/// While `timed` is unreachable, try again every five seconds.
const ZONE_RETRY_TICKS: u64 = 500;
/// The desktop launchers are re-read every ten seconds.
const LAUNCHER_TICKS: u64 = 1000;
/// A failed taskbar panel is retried every five seconds.
const BAR_RETRY_TICKS: u64 = 500;
/// A failed service registration (the old owner not reaped yet) is retried
/// every two seconds.
const REGISTER_RETRY_TICKS: u64 = 200;

/// The shell's periodic chores.
pub struct Heartbeat {
    zone: Option<String>,
    next_zone: u64,
    next_launchers: u64,
    next_bar: u64,
    next_register: u64,
    reported: bool,
}

impl Heartbeat {
    pub fn new() -> Heartbeat {
        let now = sys::clock_ticks();
        Heartbeat {
            zone: None,
            next_zone: 0,
            next_launchers: 0,
            next_bar: now.saturating_add(BAR_RETRY_TICKS),
            next_register: 0,
            reported: false,
        }
    }

    /// Refresh the clock text; `true` when it changed.
    pub fn clock(&mut self, ctx: &Ctx) -> bool {
        let now = sys::clock_ticks();
        if now >= self.next_zone {
            match services::zone() {
                Ok(zone) => {
                    self.zone = Some(zone);
                    self.next_zone = now.saturating_add(ZONE_TICKS);
                }
                Err(_) => self.next_zone = now.saturating_add(ZONE_RETRY_TICKS),
            }
        }
        let unix = lazyshell::clock::unix_from_centis(sys::wall_centis());
        let text = lazyshell::clock::text(unix, self.zone.as_deref());
        let changed = *ctx.clock.borrow() != text;
        if changed {
            *ctx.clock.borrow_mut() = text;
        }
        changed
    }

    /// Re-read the theme when due; `true` when it changed.
    pub fn theme(&mut self, ctx: &Ctx) -> bool {
        ctx.theme.borrow_mut().poll()
    }

    /// Whether the launchers are due for a re-read.
    pub fn launchers_due(&mut self) -> bool {
        due(&mut self.next_launchers, LAUNCHER_TICKS)
    }

    /// Whether a missing taskbar should be tried again now.
    pub fn retry_bar(&mut self) -> bool {
        due(&mut self.next_bar, BAR_RETRY_TICKS)
    }

    /// Whether the service registration should be tried (again) now.
    pub fn register_due(&mut self) -> bool {
        due(&mut self.next_register, REGISTER_RETRY_TICKS)
    }

    /// Print `SHELL:DESKTOP:PASS` once the desktop and the taskbar have both
    /// committed a frame.
    pub fn report_first_frame(&mut self, ctx: &Ctx, icons: usize) {
        if !self.reported && ctx.bar.borrow().is_some() && ctx.backend.frames() >= 2 {
            self.reported = true;
            println!("SHELL:DESKTOP:PASS icons={icons}");
        }
    }
}

impl Default for Heartbeat {
    fn default() -> Heartbeat {
        Heartbeat::new()
    }
}

/// Whether `next` has passed; if so, schedule the next one `period` on.
fn due(next: &mut u64, period: u64) -> bool {
    let now = sys::clock_ticks();
    if now < *next {
        return false;
    }
    *next = now.saturating_add(period);
    true
}
