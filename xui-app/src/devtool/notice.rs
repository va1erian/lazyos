//! Whether the developer-tool strip is showing: the user's choice, remembered
//! per user and per tool.
//!
//! The strip ([`super::strip`]) says what a tool's authority is. It is one
//! line of the standing warning, so a user who has read it may close it; that
//! hides the band and nothing else (the title keeps "(developer tool)", the
//! Help page keeps the whole table, and a tool's View menu brings the band back
//! with "Show developer-tool notice"). It is **not** the Installer's consent
//! band, which is the install-time consent and stays.
//!
//! The choice is a marker file in the tool's own data folder,
//! `$HOME/.apps/<system_name>/devtool-notice-dismissed`, the place the repo's
//! apps keep per-user state (`fhs::app_data_dir`): per user because `$HOME` is,
//! per tool because the folder is, so one tool's choice never hides another's.
//! It holds no secret and nothing to parse; its existence is the choice.
//!
//! A [`Notice`] is shared (an `Rc`) by every window of one tool, so they agree.
//! A tool with two windows asks the other one to re-lay out after a change.

use std::cell::Cell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The marker file's name inside the tool's data folder.
pub const MARKER: &str = "devtool-notice-dismissed";

/// Where the user's choice lives. [`FileStore`] is the real one; a test uses
/// a store in memory.
pub trait Store {
    /// Whether the user dismissed the notice.
    fn dismissed(&self) -> bool;
    /// Remember (`true`) or forget (`false`) the dismissal.
    fn set(&self, dismissed: bool) -> io::Result<()>;
}

/// The marker file of one tool.
pub struct FileStore {
    path: PathBuf,
}

/// The marker's path for the tool called `system_name` under `home`.
pub fn marker_path(home: &Path, system_name: &str) -> PathBuf {
    PathBuf::from(fhs::app_data_dir(&home.to_string_lossy(), system_name)).join(MARKER)
}

impl FileStore {
    /// The store of `system_name` under `home`.
    pub fn new(home: &Path, system_name: &str) -> FileStore {
        FileStore {
            path: marker_path(home, system_name),
        }
    }
}

impl Store for FileStore {
    fn dismissed(&self) -> bool {
        self.path.is_file()
    }

    fn set(&self, dismissed: bool) -> io::Result<()> {
        if dismissed {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&self.path, b"dismissed\n")
        } else {
            match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Ok(()),
            }
        }
    }
}

/// The notice's state: visible until dismissed, visible again after
/// [`Notice::restore`].
pub struct Notice {
    store: Box<dyn Store>,
    dismissed: Cell<bool>,
}

impl Notice {
    /// The state a store holds.
    pub fn new(store: Box<dyn Store>) -> Rc<Notice> {
        let dismissed = Cell::new(store.dismissed());
        Rc::new(Notice { store, dismissed })
    }

    /// The notice of the tool called `system_name`, for the user whose home is
    /// `$HOME` (the folder pickers use, `/transient` without one).
    pub fn for_tool(system_name: &str) -> Rc<Notice> {
        let home = crate::platform::dirs::default_dir();
        Notice::new(Box::new(FileStore::new(&home, system_name)))
    }

    /// Whether the strip shows.
    pub fn visible(&self) -> bool {
        !self.dismissed.get()
    }

    /// Hide the strip. It stays hidden for this run even when the choice
    /// cannot be written; the error says it will not be remembered.
    pub fn dismiss(&self) -> io::Result<()> {
        self.dismissed.set(true);
        self.store.set(true)
    }

    /// Show the strip again and forget the choice.
    pub fn restore(&self) -> io::Result<()> {
        self.dismissed.set(false);
        self.store.set(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A store in memory that can refuse writes.
    struct Mem {
        value: Rc<Cell<bool>>,
        writes: Rc<RefCell<Vec<bool>>>,
        refuse: bool,
    }

    impl Store for Mem {
        fn dismissed(&self) -> bool {
            self.value.get()
        }

        fn set(&self, dismissed: bool) -> io::Result<()> {
            if self.refuse {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "read-only"));
            }
            self.value.set(dismissed);
            self.writes.borrow_mut().push(dismissed);
            Ok(())
        }
    }

    fn mem(value: bool, refuse: bool) -> (Rc<Notice>, Rc<Cell<bool>>, Rc<RefCell<Vec<bool>>>) {
        let cell = Rc::new(Cell::new(value));
        let writes = Rc::new(RefCell::new(Vec::new()));
        let notice = Notice::new(Box::new(Mem {
            value: Rc::clone(&cell),
            writes: Rc::clone(&writes),
            refuse,
        }));
        (notice, cell, writes)
    }

    #[test]
    fn a_new_tool_shows_the_notice_until_the_user_dismisses_it() {
        let (notice, stored, _) = mem(false, false);
        assert!(notice.visible());
        notice.dismiss().unwrap();
        assert!(!notice.visible());
        assert!(stored.get(), "the choice reached the store");
        notice.restore().unwrap();
        assert!(notice.visible());
        assert!(!stored.get(), "restoring clears the stored choice");
    }

    #[test]
    fn a_stored_dismissal_is_honoured_at_start() {
        let (notice, _, writes) = mem(true, false);
        assert!(!notice.visible());
        assert!(writes.borrow().is_empty(), "reading writes nothing");
    }

    #[test]
    fn a_second_window_of_the_tool_follows_the_first() {
        let (a, stored, _) = mem(false, false);
        let b = Rc::clone(&a);
        a.dismiss().unwrap();
        assert!(!b.visible());
        // A later launch reads what the first left.
        let (later, _, _) = mem(stored.get(), false);
        assert!(!later.visible());
    }

    #[test]
    fn a_choice_that_cannot_be_written_still_applies_to_this_run() {
        let (notice, _, _) = mem(false, true);
        assert!(notice.dismiss().is_err());
        assert!(!notice.visible(), "hidden now, forgotten next launch");
        assert!(notice.restore().is_err());
        assert!(notice.visible(), "showing it again never depends on the disk");
    }

    #[test]
    fn the_marker_is_per_user_and_per_tool() {
        let one = marker_path(Path::new("/home/user"), "os.lazy.playground");
        let other_tool = marker_path(Path::new("/home/user"), "os.lazy.other");
        let other_user = marker_path(Path::new("/home/admin"), "os.lazy.playground");
        assert_eq!(
            one,
            PathBuf::from("/home/user/.apps/os.lazy.playground/devtool-notice-dismissed")
        );
        assert_ne!(one, other_tool);
        assert_ne!(one, other_user);
    }

    #[test]
    fn the_file_store_round_trips_and_forgetting_twice_is_fine() {
        let dir = std::env::temp_dir().join(format!("lazyos-notice-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = FileStore::new(&dir, "os.lazy.playground");
        assert!(!store.dismissed());
        store.set(true).unwrap();
        assert!(store.dismissed(), "the folder is made on the first write");
        store.set(false).unwrap();
        assert!(!store.dismissed());
        store.set(false).unwrap();
        // Another tool's folder is untouched.
        assert!(!FileStore::new(&dir, "os.lazy.other").dismissed());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
