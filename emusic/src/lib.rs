//! `lazyemusic`: emusic as a LazyOS desktop app (docs/media-plan.md).
//!
//! emusic's own crates do the work: its portable frontend draws the window
//! through `xui-app`'s backend and its `emusic-lazyaudio` backend decodes the
//! MP3s. This crate supplies the LazyOS half: the `audiod` output
//! ([`audiod`]), serial evidence ([`markers`]), the headless sound check
//! ([`soundcheck`]) and the process ([`app`]).

#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod app;
#[cfg(all(target_os = "linux", target_env = "musl"))]
pub mod audiod;
pub mod markers;
pub mod soundcheck;

/// The package's `system_name`: its per-user folder is named after it.
pub const SYSTEM_NAME: &str = "org.lazy.emusic";

/// The window's size in design pixels: most of a 1280x720 desktop, leaving
/// the taskbar.
pub const WINDOW_SIZE: (f32, f32) = (1040.0, 620.0);

/// Where emusic keeps its config, library database and thumbnails: the
/// package's folder in the user's home, `$HOME/.apps/org.lazy.emusic` (the one
/// place its manifest may write), or `/tmp/emusic` without a home.
pub fn data_dir(home: Option<&str>) -> String {
    match home.filter(|home| home.starts_with('/') && *home != "/") {
        Some(home) => fhs::app_data_dir(home, SYSTEM_NAME),
        None => format!("{}/emusic", fhs::mount::TMP),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_lives_in_the_package_folder() {
        assert_eq!(
            data_dir(Some("/home/user")),
            "/home/user/.apps/org.lazy.emusic"
        );
        assert_eq!(data_dir(Some("/")), format!("{}/emusic", fhs::mount::TMP));
        assert_eq!(
            data_dir(Some("relative")),
            format!("{}/emusic", fhs::mount::TMP)
        );
        assert_eq!(data_dir(None), format!("{}/emusic", fhs::mount::TMP));
    }
}
