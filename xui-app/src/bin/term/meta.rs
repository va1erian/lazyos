//! What a program tells the window about itself: its title (OSC 0 and 2), the
//! working directory its shell reports (OSC 7), and the synchronized updates
//! (`?2026`) it asks the repaint to wait for.

use xui_core::app::Ui;

use super::appearance::now_ms;
use super::{Msg, Terminal};

/// The title when no program has set one.
pub const DEFAULT_TITLE: &str = "Terminal";
/// How long a synchronized update may hold the repaint: a program that never
/// ends one cannot freeze the view past this.
pub const SYNC_HOLD_MS: u32 = 150;

/// The shell's directory as a path: `file://host/path` with the host the
/// prompt's `\w` gave. BusyBox abbreviates the home directory to `~`, which
/// arrives as the host, so it is expanded here.
pub fn resolve_cwd(host: &str, path: &str, home: Option<&str>) -> Option<String> {
    if host == "~" {
        let home = home.filter(|h| h.starts_with('/'))?;
        return Some(format!("{}{path}", home.trim_end_matches('/')));
    }
    path.starts_with('/').then(|| path.to_string())
}

impl Terminal {
    /// Show a changed title and report a changed working directory.
    pub(super) fn meta_changed(&mut self, ui: &mut Ui<Msg>) {
        let (generation, title, cwd) = {
            let term = self.term.borrow();
            (term.title_generation(), term.title().map(str::to_string), term.cwd().cloned())
        };
        if generation != self.title_generation {
            self.title_generation = generation;
            let shown = title.as_deref().unwrap_or(DEFAULT_TITLE);
            ui.set_window_title(shown);
            println!("TERM:TITLE:{shown}");
        }
        if cwd != self.cwd {
            if let Some(cwd) = &cwd {
                let home = std::env::var("HOME").ok();
                if let Some(path) = resolve_cwd(&cwd.host, &cwd.path, home.as_deref()) {
                    println!("TERM:CWD:{path}");
                }
            }
            self.cwd = cwd;
        }
    }

    /// The shell's working directory, for opening a window in it.
    #[allow(dead_code)]
    pub(super) fn cwd_path(&self) -> Option<String> {
        let cwd = self.cwd.as_ref()?;
        resolve_cwd(&cwd.host, &cwd.path, std::env::var("HOME").ok().as_deref())
    }

    /// Whether the repaint waits: a synchronized update is open and has not
    /// been held longer than [`SYNC_HOLD_MS`].
    pub(super) fn hold_for_sync(&mut self, ui: &mut Ui<Msg>) -> bool {
        if !self.term.borrow().sync_output() {
            self.sync_hold = None;
            if let Some(id) = self.sync_timer.take() {
                ui.kill_timer(id);
            }
            return false;
        }
        let now = now_ms();
        match self.sync_hold {
            None => {
                self.sync_hold = Some(now);
                if self.sync_timer.get().is_none() {
                    self.sync_timer.set(Some(ui.set_timer(SYNC_HOLD_MS)));
                }
                true
            }
            Some(since) => now.saturating_sub(since) < u64::from(SYNC_HOLD_MS),
        }
    }

    /// The hold ran out: paint what the program has drawn so far.
    pub(super) fn sync_expired(&mut self, ui: &mut Ui<Msg>) {
        self.sync_hold = None;
        if let Some(id) = self.sync_timer.take() {
            ui.kill_timer(id);
        }
        self.repaint(ui);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_directory_resolves_the_tilde() {
        assert_eq!(resolve_cwd("", "/tmp/a", None).as_deref(), Some("/tmp/a"));
        assert_eq!(resolve_cwd("lazyos", "/", None).as_deref(), Some("/"));
        assert_eq!(resolve_cwd("~", "/src", Some("/home/user")).as_deref(), Some("/home/user/src"));
        assert_eq!(resolve_cwd("~", "", Some("/home/user/")).as_deref(), Some("/home/user"));
        assert_eq!(resolve_cwd("~", "/x", None), None, "no home to expand");
        assert_eq!(resolve_cwd("~", "/x", Some("relative")), None);
        assert_eq!(resolve_cwd("", "relative", None), None);
    }
}
