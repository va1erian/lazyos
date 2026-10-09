#![forbid(unsafe_code)]

//! A window's navigation history: the folders behind it (Back) and ahead of
//! it (Forward), like a browser's.

use std::path::{Path, PathBuf};

/// The most folders kept in each direction; the oldest are dropped.
const LIMIT: usize = 100;

/// The Back and Forward stacks around the window's current folder.
#[derive(Clone, Debug, Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
}

impl History {
    /// An empty history.
    pub fn new() -> History {
        History::default()
    }

    /// The window left `from` for a new folder: `from` goes on the Back stack
    /// and the Forward stack is cleared, as a browser does after a new visit.
    pub fn visit(&mut self, from: &Path) {
        push_bounded(&mut self.back, from.to_path_buf());
        self.forward.clear();
    }

    /// Steps back from `current`: returns the folder to show and puts
    /// `current` on the Forward stack, or `None` when there is nothing behind.
    pub fn back(&mut self, current: &Path) -> Option<PathBuf> {
        let target = self.back.pop()?;
        push_bounded(&mut self.forward, current.to_path_buf());
        Some(target)
    }

    /// Steps forward from `current`; the mirror of [`back`](Self::back).
    pub fn forward(&mut self, current: &Path) -> Option<PathBuf> {
        let target = self.forward.pop()?;
        push_bounded(&mut self.back, current.to_path_buf());
        Some(target)
    }

    /// Whether Back has somewhere to go.
    pub fn can_back(&self) -> bool {
        !self.back.is_empty()
    }

    /// Whether Forward has somewhere to go.
    pub fn can_forward(&self) -> bool {
        !self.forward.is_empty()
    }
}

/// Pushes `path` and drops the oldest entry past [`LIMIT`].
fn push_bounded(stack: &mut Vec<PathBuf>, path: PathBuf) {
    stack.push(path);
    if stack.len() > LIMIT {
        stack.remove(0);
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{History, LIMIT};

    #[test]
    fn back_and_forward_walk_the_visits() {
        let mut history = History::new();
        assert!(!history.can_back() && !history.can_forward());
        history.visit(Path::new("/a"));
        history.visit(Path::new("/a/b"));
        // Now at /a/b/c.
        assert_eq!(
            history.back(Path::new("/a/b/c")),
            Some(PathBuf::from("/a/b"))
        );
        assert_eq!(history.back(Path::new("/a/b")), Some(PathBuf::from("/a")));
        assert_eq!(history.back(Path::new("/a")), None, "nothing behind /a");
        assert!(history.can_forward());
        assert_eq!(
            history.forward(Path::new("/a")),
            Some(PathBuf::from("/a/b"))
        );
        assert_eq!(
            history.forward(Path::new("/a/b")),
            Some(PathBuf::from("/a/b/c"))
        );
        assert!(!history.can_forward());
    }

    #[test]
    fn a_new_visit_clears_forward() {
        let mut history = History::new();
        history.visit(Path::new("/a"));
        history.back(Path::new("/b"));
        assert!(history.can_forward());
        history.visit(Path::new("/a"));
        assert!(
            !history.can_forward(),
            "a new visit drops the forward stack"
        );
        assert!(history.can_back());
    }

    #[test]
    fn the_stacks_are_bounded() {
        let mut history = History::new();
        for index in 0..LIMIT + 10 {
            history.visit(Path::new(&format!("/{index}")));
        }
        let mut steps = 0;
        let mut current = PathBuf::from("/end");
        while let Some(previous) = history.back(&current) {
            current = previous;
            steps += 1;
        }
        assert_eq!(steps, LIMIT);
        assert_eq!(
            current,
            PathBuf::from("/10"),
            "the oldest visits were dropped"
        );
    }
}
