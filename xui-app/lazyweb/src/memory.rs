//! Keeping the page's data in memory (issue #940): cookies, the HTTP cache and site storage live in
//! WebKit's non-persistent data store and are gone when the browser quits. On a PC whose `/home` is
//! a USB stick the network process otherwise stalls on the stick for every cache and cookie write,
//! and the page with it.
//!
//! The choice is a file in the profile folder ([`MARKER`]): the UI process reads it before WebKit
//! starts, which is before it can reach `confd`, and the File menu writes or removes it. It takes
//! effect at the next start. The profile folder keeps what is not the page's data: the history, the
//! notification choices and the ad blocker's compiled lists.

use std::io;
use std::path::Path;

/// The file whose presence in the profile folder keeps the page's data in memory.
pub const MARKER: &str = "page-data-in-memory";

/// Whether the profile at `profile` asks for the page's data to be kept in memory.
pub fn enabled(profile: &Path) -> bool {
    profile.join(MARKER).is_file()
}

/// Ask (or stop asking) for the page's data to be kept in memory from the next start.
pub fn set(profile: &Path, on: bool) -> io::Result<()> {
    let marker = profile.join(MARKER);
    if on {
        std::fs::write(
            marker,
            b"LazyWeb keeps cookies, the cache and site data in memory (File menu).\n",
        )
    } else {
        match std::fs::remove_file(marker) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

/// What the status bar says after the File menu's item flipped it to `on`.
pub fn status(on: bool) -> &'static str {
    if on {
        "From the next start, cookies, the cache and site data are kept in memory only"
    } else {
        "From the next start, cookies, the cache and site data are kept in your profile"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lazyweb-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_marker_turns_it_on_and_off() {
        let dir = scratch("toggle");
        assert!(!enabled(&dir), "off by default");
        set(&dir, true).unwrap();
        assert!(enabled(&dir));
        set(&dir, true).unwrap();
        assert!(enabled(&dir), "on twice is on");
        set(&dir, false).unwrap();
        assert!(!enabled(&dir));
        set(&dir, false).unwrap();
        assert!(!enabled(&dir), "off twice is off, not an error");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_directory_of_that_name_is_not_the_marker() {
        let dir = scratch("dir");
        std::fs::create_dir(dir.join(MARKER)).unwrap();
        assert!(!enabled(&dir));
        assert!(
            set(&dir, false).is_err(),
            "a directory is not removed as a marker"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_profile_cannot_be_switched_on() {
        let dir = scratch("missing").join("gone");
        assert!(set(&dir, true).is_err());
        assert!(!enabled(&dir));
    }
}
