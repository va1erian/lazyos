//! Links LazyWeb cannot follow itself (`mailto:`, other schemes), handed to
//! the app the system registered for them.
//!
//! Serial evidence: `WEB:LAUNCH:<url>:OK|FAIL|BLOCKED`.

use lazyweb::marker_text;
use xui_app::platform::launcher;

/// Hands `url` to its app; the status line to show. A page may not start
/// another program on its own (a refresh or a script): only a click, a key
/// or a typed address (`by_user`) may.
pub fn open(url: &str, by_user: bool) -> String {
    if !by_user {
        println!("WEB:LAUNCH:{}:BLOCKED", marker_text(url));
        return format!("Blocked {url}: the page tried to open another app");
    }
    match launcher::open_url(url) {
        Ok(()) => {
            println!("WEB:LAUNCH:{}:OK", marker_text(url));
            format!("Opened {url} in another app")
        }
        Err(e) => {
            println!("WEB:LAUNCH:{}:FAIL", marker_text(url));
            format!("Cannot open {url}: {e}")
        }
    }
}
