//! The Back/Forward list, kept by the app.
//!
//! NetSurf keeps a history of its own, but the view does not expose it, so
//! the list is built from the URLs the view reports: each load the user
//! starts (a typed address or a followed link) adds an entry, a redirect
//! within that load replaces it, and Back, Forward and Reload move along the
//! list without adding anything.

/// Visited URLs and where we are among them.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    index: usize,
    /// A Back/Forward/Reload load is under way: its URLs replace the entry.
    jumping: bool,
    /// The current load already added an entry; a later URL in the same load
    /// is a redirect.
    added_this_load: bool,
}

impl History {
    pub fn new() -> History {
        History::default()
    }

    /// The entry on show.
    pub fn current(&self) -> Option<&str> {
        self.entries.get(self.index).map(String::as_str)
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.entries.len()
    }

    /// The view moved to `url` (a load started, followed a link or a
    /// redirect).
    pub fn on_url(&mut self, url: &str) {
        if self.current() == Some(url) {
            return;
        }
        if self.entries.is_empty() {
            self.entries.push(url.to_string());
            self.index = 0;
        } else if self.jumping || self.added_this_load {
            self.entries[self.index] = url.to_string();
        } else {
            self.entries.truncate(self.index + 1);
            self.entries.push(url.to_string());
            self.index += 1;
        }
        self.added_this_load = true;
    }

    /// The view finished (or gave up) a load.
    pub fn on_load_end(&mut self) {
        self.jumping = false;
        self.added_this_load = false;
    }

    /// Steps back; returns the URL to open.
    pub fn back(&mut self) -> Option<String> {
        self.can_go_back().then(|| self.jump(self.index - 1))
    }

    /// Steps forward; returns the URL to open.
    pub fn forward(&mut self) -> Option<String> {
        self.can_go_forward().then(|| self.jump(self.index + 1))
    }

    /// The URL to open again.
    pub fn reload(&mut self) -> Option<String> {
        self.current().is_some().then(|| self.jump(self.index))
    }

    fn jump(&mut self, to: usize) -> String {
        self.index = to;
        self.jumping = true;
        self.added_this_load = false;
        self.entries[to].clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visit(h: &mut History, url: &str) {
        h.on_url(url);
        h.on_load_end();
    }

    #[test]
    fn back_and_forward_walk_the_list() {
        let mut h = History::new();
        visit(&mut h, "a");
        visit(&mut h, "b");
        visit(&mut h, "c");
        assert_eq!(h.back().as_deref(), Some("b"));
        visit(&mut h, "b");
        assert_eq!(h.back().as_deref(), Some("a"));
        visit(&mut h, "a");
        assert!(!h.can_go_back());
        assert_eq!(h.forward().as_deref(), Some("b"));
        visit(&mut h, "b");
        assert!(h.can_go_forward());
    }

    #[test]
    fn a_new_visit_drops_the_forward_entries() {
        let mut h = History::new();
        for url in ["a", "b", "c"] {
            visit(&mut h, url);
        }
        h.back();
        visit(&mut h, "b");
        visit(&mut h, "d");
        assert!(!h.can_go_forward());
        assert_eq!(h.back().as_deref(), Some("b"));
    }

    #[test]
    fn redirects_replace_the_entry() {
        let mut h = History::new();
        visit(&mut h, "a");
        h.on_url("http://old/");
        h.on_url("http://new/");
        h.on_load_end();
        assert_eq!(h.current(), Some("http://new/"));
        assert_eq!(h.back().as_deref(), Some("a"));
        visit(&mut h, "a");
        assert_eq!(h.forward().as_deref(), Some("http://new/"));
    }

    #[test]
    fn going_back_to_a_redirect_keeps_the_forward_entries() {
        let mut h = History::new();
        visit(&mut h, "a");
        visit(&mut h, "b");
        assert_eq!(h.back().as_deref(), Some("a"));
        h.on_url("a2");
        h.on_load_end();
        assert_eq!(h.current(), Some("a2"));
        assert!(h.can_go_forward());
    }

    #[test]
    fn reload_changes_nothing() {
        let mut h = History::new();
        assert_eq!(h.reload(), None);
        visit(&mut h, "a");
        assert_eq!(h.reload().as_deref(), Some("a"));
        visit(&mut h, "a");
        assert!(!h.can_go_back() && !h.can_go_forward());
    }
}
