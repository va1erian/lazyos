//! `xui-shell`: LazyShell, the LazyOS desktop shell (issue #157).
//!
//! The desktop (wallpaper and launcher icons), the taskbar ("LazyOS" button,
//! window entries, clock) and the start menu, drawn as `xuid` desktop and
//! panel surfaces, plus the `os.lazy.shell` service. `init` starts it on the
//! desktop image with `--client` and restarts it if it dies; a restarted
//! shell rebuilds its taskbar from the compositor's window list. See
//! `xui_app::shell` for the start-up order and the serial markers.

fn main() {
    std::process::exit(xui_app::shell::run());
}
