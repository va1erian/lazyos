#![forbid(unsafe_code)]

//! The path-to-window registry behind the shell, and the [`Closable`] seam that
//! lets it be tested without a window.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use xui_core::app::WindowHandle;

use crate::model::is_within;

/// A window the registry can close and ask whether it is still open.
///
/// A real entry is a [`WindowHandle`]; the trait is small so the registry can
/// be tested without opening a window.
pub trait Closable {
    /// Closes the window. Safe to call more than once.
    fn close(&self);
    /// Whether the window is still open.
    fn is_open(&self) -> bool;
}

impl<M: 'static> Closable for WindowHandle<M> {
    fn close(&self) {
        WindowHandle::close(self);
    }

    fn is_open(&self) -> bool {
        WindowHandle::is_open(self)
    }
}

/// The path-to-window map. `None` marks the primary window, which the registry
/// tracks for duplicate detection but can never close.
pub struct Registry<W> {
    entries: RefCell<Vec<(PathBuf, Option<W>)>>,
}

impl<W> Default for Registry<W> {
    fn default() -> Registry<W> {
        Registry {
            entries: RefCell::new(Vec::new()),
        }
    }
}

impl<W> Registry<W> {
    /// An empty registry.
    pub fn new() -> Registry<W> {
        Registry::default()
    }

    /// Records `path` as shown by a window the registry cannot close (the
    /// primary/root window).
    pub fn register_primary(&self, path: PathBuf) {
        self.entries.borrow_mut().push((path, None));
    }

    /// Records `handle` as showing `path`.
    pub fn register(&self, path: PathBuf, handle: W) {
        self.entries.borrow_mut().push((path, Some(handle)));
    }

    /// The number of tracked windows, including the primary one.
    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    /// Whether the registry tracks no windows.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<W: Closable> Registry<W> {
    /// Drops entries whose window has closed.
    pub fn prune(&self) {
        self.entries
            .borrow_mut()
            .retain(|(_, handle)| handle.as_ref().is_none_or(Closable::is_open));
    }

    /// Whether a window is showing `path`.
    pub fn is_open(&self, path: &Path) -> bool {
        self.prune();
        self.entries
            .borrow()
            .iter()
            .any(|(entry, handle)| entry == path && handle.as_ref().is_none_or(Closable::is_open))
    }

    /// The paths of every tracked window at or below `dir`.
    pub fn paths_under(&self, dir: &Path) -> Vec<PathBuf> {
        self.entries
            .borrow()
            .iter()
            .filter(|(path, _)| is_within(path, dir))
            .map(|(path, _)| path.clone())
            .collect()
    }

    /// Closes every secondary window showing `dir` or a folder below it. The
    /// registry borrow is released before any handle is closed.
    pub fn close_under(&self, dir: &Path) {
        let mut closed: Vec<W> = Vec::new();
        {
            let mut entries = self.entries.borrow_mut();
            entries.retain_mut(|(path, handle)| {
                if is_within(path, dir) && handle.is_some() {
                    closed.push(handle.take().expect("checked above"));
                    false
                } else {
                    true
                }
            });
        }
        for handle in closed {
            handle.close();
        }
    }
}

impl<W: Closable + Clone> Registry<W> {
    /// Clones the handle of every open window showing `path` exactly. The
    /// borrow is released before the caller sends anything.
    pub fn handles_at(&self, path: &Path) -> Vec<W> {
        self.prune();
        self.entries
            .borrow()
            .iter()
            .filter(|(entry, _)| entry == path)
            .filter_map(|(_, handle)| handle.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::{Closable, Registry};

    /// A closable stand-in so the registry is tested without a window.
    #[derive(Clone)]
    struct Fake {
        open: Rc<Cell<bool>>,
    }

    impl Fake {
        fn open() -> Fake {
            Fake {
                open: Rc::new(Cell::new(true)),
            }
        }

        fn dead(&self) {
            self.open.set(false);
        }
    }

    impl Closable for Fake {
        fn close(&self) {
            self.open.set(false);
        }

        fn is_open(&self) -> bool {
            self.open.get()
        }
    }

    #[test]
    fn a_dead_entry_is_pruned_and_not_reported_open() {
        let registry: Registry<Fake> = Registry::new();
        let window = Fake::open();
        registry.register(PathBuf::from("/a"), window.clone());
        assert!(registry.is_open(Path::new("/a")));
        window.dead();
        assert!(!registry.is_open(Path::new("/a")));
        assert!(registry.is_empty(), "the dead entry was pruned");
    }

    #[test]
    fn closing_under_closes_every_descendant_and_not_the_sibling() {
        let registry: Registry<Fake> = Registry::new();
        let a = Fake::open();
        let b = Fake::open();
        let sibling = Fake::open();
        registry.register(PathBuf::from("/a"), a.clone());
        registry.register(PathBuf::from("/a/b"), b.clone());
        registry.register(PathBuf::from("/ab"), sibling.clone());

        registry.close_under(Path::new("/a"));

        assert!(!a.is_open(), "the folder itself closed");
        assert!(!b.is_open(), "its descendant closed");
        assert!(sibling.is_open(), "/ab is not under /a");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_primary_entry_is_never_closed() {
        let registry: Registry<Fake> = Registry::new();
        let child = Fake::open();
        registry.register_primary(PathBuf::from("/a"));
        registry.register(PathBuf::from("/a/b"), child.clone());

        registry.close_under(Path::new("/a"));

        assert!(!child.open.get(), "the secondary window closed");
        assert!(
            registry.is_open(Path::new("/a")),
            "the primary window is still tracked"
        );
    }

    #[test]
    fn handles_at_returns_only_the_exact_folder() {
        let registry: Registry<Fake> = Registry::new();
        registry.register(PathBuf::from("/a"), Fake::open());
        registry.register(PathBuf::from("/a/b"), Fake::open());
        assert_eq!(registry.handles_at(Path::new("/a")).len(), 1);
        assert_eq!(registry.paths_under(Path::new("/a")).len(), 2);
    }
}
