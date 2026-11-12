//! The files an app opened or saved last, newest first, kept through
//! [`Settings`] (`user/<uid>/<app>/recent`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::{KeyValue, Saver, Settings};

/// How many files are remembered.
pub const MAX: usize = 10;

#[derive(Serialize, Deserialize, Default)]
struct Stored {
    recent: Vec<String>,
}

/// An app's recent files.
pub struct Recent(Settings<Stored>);

impl Recent {
    /// Opens the list of `app`.
    pub fn open(kv: &dyn KeyValue, app: &str, saver: Saver) -> Recent {
        Recent(Settings::open(kv, app, saver))
    }

    /// The remembered files, newest first.
    pub fn list(&self) -> Vec<PathBuf> {
        self.0.get().recent.iter().map(PathBuf::from).collect()
    }

    /// Remembers `path` as the newest, once, and forgets the oldest beyond
    /// [`MAX`].
    pub fn push(&self, path: &Path) {
        let Some(text) = path.to_str() else {
            return;
        };
        self.0.update(|stored| {
            stored.recent.retain(|known| known != text);
            stored.recent.insert(0, text.to_owned());
            stored.recent.truncate(MAX);
        });
    }

    /// Forgets `path` (a file that is gone).
    pub fn remove(&self, path: &Path) {
        self.0
            .update(|stored| stored.recent.retain(|known| Path::new(known) != path));
    }

    /// The folder of the newest file: where a picker should open next.
    pub fn last_dir(&self) -> Option<PathBuf> {
        let newest = self.list().into_iter().next()?;
        newest.parent().map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::MemKv;
    use std::rc::Rc;

    fn recent(kv: &Rc<MemKv>) -> Recent {
        Recent::open(kv.as_ref(), "demo", Saver::Inline(kv.clone()))
    }

    #[test]
    fn the_newest_is_first_and_a_repeat_moves_up() {
        let kv = Rc::new(MemKv::new(1));
        let recent = recent(&kv);
        for name in ["/a/1", "/a/2", "/b/3", "/a/1"] {
            recent.push(Path::new(name));
        }
        assert_eq!(
            recent.list(),
            [Path::new("/a/1"), Path::new("/b/3"), Path::new("/a/2")]
        );
        assert_eq!(recent.last_dir(), Some(PathBuf::from("/a")));
    }

    #[test]
    fn the_list_is_bounded_and_survives_a_restart() {
        let kv = Rc::new(MemKv::new(1));
        let first = recent(&kv);
        for n in 0..25 {
            first.push(Path::new(&format!("/f/{n}")));
        }
        let again = recent(&kv);
        assert_eq!(again.list().len(), MAX);
        assert_eq!(again.list()[0], Path::new("/f/24"));
        again.remove(Path::new("/f/24"));
        assert_eq!(again.list()[0], Path::new("/f/23"));
    }
}
