//! Links the engine does not load (`mailto:`, other schemes), handed to the app the system
//! registered for them (`mimed`).
//!
//! Serial evidence: `WEBPLUS:LAUNCH:<url>:OK|FAIL|BLOCKED`.

use crate::marker_text;

/// Starts another program for a URL.
pub trait Launcher {
    /// Opens `url` in the app registered for its scheme.
    fn open_url(&self, url: &str) -> Result<(), String>;
    /// Opens the file at `path` with the app registered for its type.
    fn open_path(&self, path: &str) -> Result<(), String>;
}

/// Hands `url` to its app; the status line to show. A page may not start another program on
/// its own (a script, a redirect, a refresh): only a click, a key or a typed address (`by_user`)
/// may. The navigation policy already refuses what a script starts (`policy.rs`); this is the
/// second lock on the door.
pub fn open(launcher: &dyn Launcher, url: &str, by_user: bool) -> String {
    if !by_user {
        println!("WEBPLUS:LAUNCH:{}:BLOCKED", marker_text(url));
        return format!("Blocked {url}: the page tried to open another app");
    }
    match launcher.open_url(url) {
        Ok(()) => {
            println!("WEBPLUS:LAUNCH:{}:OK", marker_text(url));
            format!("Opened {url} in another app")
        }
        Err(e) => {
            println!("WEBPLUS:LAUNCH:{}:FAIL", marker_text(url));
            format!("Cannot open {url}: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Recording {
        opened: RefCell<Vec<String>>,
        fail: bool,
    }

    impl Launcher for Recording {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.opened.borrow_mut().push(url.to_owned());
            if self.fail {
                Err("no handler".into())
            } else {
                Ok(())
            }
        }
        fn open_path(&self, path: &str) -> Result<(), String> {
            self.open_url(path)
        }
    }

    #[test]
    fn a_user_action_starts_the_app() {
        let launcher = Recording::default();
        let status = open(&launcher, "mailto:a@b.test", true);
        assert_eq!(*launcher.opened.borrow(), ["mailto:a@b.test"]);
        assert!(status.starts_with("Opened"));
    }

    #[test]
    fn a_page_on_its_own_starts_nothing() {
        let launcher = Recording::default();
        let status = open(&launcher, "mailto:a@b.test", false);
        assert!(launcher.opened.borrow().is_empty());
        assert!(status.starts_with("Blocked"));
    }

    #[test]
    fn a_missing_handler_is_reported_not_hidden() {
        let launcher = Recording {
            fail: true,
            ..Recording::default()
        };
        assert!(open(&launcher, "tel:1", true).starts_with("Cannot open tel:1: no handler"));
    }
}
