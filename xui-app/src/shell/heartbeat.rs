//! The heartbeat's schedule: when to re-read the zone, the theme and the
//! desktop icons, and when to retry what failed (the taskbar panel, the service
//! registration). All deadlines are PIT ticks (100 Hz).

use super::ctx::Ctx;
use super::services;
use crate::sys;

/// The zone is re-read every three seconds, the cadence of the theme and
/// clock-format keys, so a zone picked in Settings shows almost at once (the
/// clock itself follows the kernel's UTC every heartbeat).
const ZONE_TICKS: u64 = 300;
/// While `timed` is unreachable, try again every five seconds.
const ZONE_RETRY_TICKS: u64 = 500;
/// `init`'s registry is re-read for the desktop icons every ten seconds
/// (hidden apps, package icons)...
const LAUNCHER_TICKS: u64 = 1000;
/// ...and the desktop folder's listing about once a second.
const FOLDER_TICKS: u64 = 100;
/// A failed taskbar panel is retried every five seconds...
const BAR_RETRY_TICKS: u64 = 500;
/// ...a few times: a compositor that refuses panels for good (one older than
/// issue #157) would otherwise flash a refused surface forever.
const BAR_RETRIES: u32 = 5;
/// A failed service registration (the old owner not reaped yet) is retried
/// every two seconds.
const REGISTER_RETRY_TICKS: u64 = 200;

/// The shell's periodic chores.
pub struct Heartbeat {
    zone: Option<String>,
    next_zone: u64,
    next_launchers: u64,
    next_folder: u64,
    next_bar: u64,
    bar_retries: u32,
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
            next_folder: 0,
            next_bar: now.saturating_add(BAR_RETRY_TICKS),
            bar_retries: 0,
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
        let format = ctx.theme.borrow().clock_format();
        let text = lazyshell::clock::text(unix, self.zone.as_deref(), format);
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

    /// Whether the desktop icons' apps are due for a re-read.
    pub fn launchers_due(&mut self) -> bool {
        due(&mut self.next_launchers, LAUNCHER_TICKS)
    }

    /// Whether the desktop folder is due for a look.
    pub fn folder_due(&mut self) -> bool {
        due(&mut self.next_folder, FOLDER_TICKS)
    }

    /// Whether a missing taskbar should be tried again now.
    pub fn retry_bar(&mut self) -> bool {
        if self.bar_retries >= BAR_RETRIES || !due(&mut self.next_bar, BAR_RETRY_TICKS) {
            return false;
        }
        self.bar_retries += 1;
        true
    }

    /// Whether the service registration should be tried (again) now.
    pub fn register_due(&mut self) -> bool {
        due(&mut self.next_register, REGISTER_RETRY_TICKS)
    }

    /// Print `SHELL:DESKTOP:PASS` once the desktop and the taskbar have both
    /// committed a frame.
    pub fn report_first_frame(&mut self, ctx: &Ctx, icons: usize) {
        if !self.reported
            && ctx.bar.borrow().is_some()
            && ctx.backend.frames() >= 2
            && ctx.desktop_loaded()
        {
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
