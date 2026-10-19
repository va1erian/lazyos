//! Starting a resident app in the background (issue #928, docs/tray-plan.md
//! section 5): a resident app that opens with the session (its manifest's
//! `autostart`) must not show its window at every login, while the same app
//! opened from the Start menu, a desktop icon or `mimed` must.
//!
//! `init` alone knows which launch is which, so it tells the app: a resident
//! app's autostart launch gets [`BACKGROUND_ARG`] on its command line, and no
//! other launch does. The app reads it with [`is_background_start`] (xui apps
//! through `xui_app::resident::started_in_background`) and starts with its
//! tray icon only; a later launch reaches it as `Reopen`, which shows the
//! window. A restart after a crash keeps the row's arguments, so an instance
//! started in the background comes back in the background.

/// The argument `init` adds to a resident app's autostart launch.
pub const BACKGROUND_ARG: &str = "--background";

/// Whether a launch starts the app in the background: only a resident app,
/// and only when `init` opens it itself (the session's autostart). A launch
/// someone asked for (the menu, an icon, `mimed`, a relaunch) shows a window.
pub fn starts_in_background(resident: bool, autostart: bool) -> bool {
    resident && autostart
}

/// Whether `args` (a program's command line, with or without the program
/// name) asks for a background start. Only the exact token counts.
pub fn is_background_start<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().any(|arg| arg.as_ref() == BACKGROUND_ARG)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_resident_autostart_starts_in_the_background() {
        assert!(starts_in_background(true, true));
        // The Start menu (or any other request) opening a resident app.
        assert!(!starts_in_background(true, false));
        // An ordinary app that autostarts keeps its window.
        assert!(!starts_in_background(false, true));
        assert!(!starts_in_background(false, false));
    }

    #[test]
    fn the_flag_is_found_anywhere_on_the_command_line() {
        assert!(is_background_start([
            "/apps/x/bin/x.elf",
            "--client",
            BACKGROUND_ARG,
            "attempt=1"
        ]));
        assert!(is_background_start([BACKGROUND_ARG]));
    }

    #[test]
    fn a_menu_launch_or_a_lookalike_is_not_a_background_start() {
        assert!(!is_background_start(["/apps/x/bin/x.elf", "--client", "attempt=1"]));
        assert!(!is_background_start(["x", "--background=1", "--backgrounds"]));
        assert!(!is_background_start(["x", "/home/user/--background"]));
        assert!(!is_background_start(Vec::<&str>::new()));
    }
}
