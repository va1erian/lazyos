//! Inode numbers for a daemon whose tree is named by paths (a network
//! share): [`Inodes`] gives each path a number the first time it is seen,
//! keeps it across renames and never reuses one, so a node handle the kernel
//! holds for a deleted file stays stale instead of reaching a new file.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// Inode numbers by path: stable while the daemon runs, moved by renames,
/// never reused (so a node handle of a deleted file stays stale).
pub struct Inodes {
    by_path: BTreeMap<String, u64>,
    by_ino: BTreeMap<u64, String>,
    next: u64,
}

impl Default for Inodes {
    fn default() -> Self {
        Inodes::new()
    }
}

impl Inodes {
    pub const ROOT: u64 = 1;

    pub fn new() -> Inodes {
        let mut inodes = Inodes {
            by_path: BTreeMap::new(),
            by_ino: BTreeMap::new(),
            next: Self::ROOT + 1,
        };
        inodes.by_path.insert(String::new(), Self::ROOT);
        inodes.by_ino.insert(Self::ROOT, String::new());
        inodes
    }

    pub fn ino(&mut self, path: &str) -> u64 {
        if let Some(&ino) = self.by_path.get(path) {
            return ino;
        }
        let ino = self.next;
        self.next += 1;
        self.by_path.insert(String::from(path), ino);
        self.by_ino.insert(ino, String::from(path));
        ino
    }

    pub fn path(&self, ino: u64) -> Option<&str> {
        self.by_ino.get(&ino).map(String::as_str)
    }

    /// Forget `path` and everything below it.
    pub fn forget(&mut self, path: &str) {
        for (old, ino) in self.below(path) {
            self.by_path.remove(&old);
            self.by_ino.remove(&ino);
        }
    }

    /// `from` (and everything below it) is now `to`.
    pub fn rename(&mut self, from: &str, to: &str) {
        self.forget(to);
        for (old, ino) in self.below(from) {
            let new = alloc::format!("{to}{}", &old[from.len()..]);
            self.by_path.remove(&old);
            self.by_path.insert(new.clone(), ino);
            self.by_ino.insert(ino, new);
        }
    }

    fn below(&self, path: &str) -> Vec<(String, u64)> {
        self.by_path
            .iter()
            .filter(|(p, _)| {
                p.as_str() == path
                    || (p.starts_with(path) && p.as_bytes().get(path.len()) == Some(&b'/'))
            })
            .map(|(p, &ino)| (p.clone(), ino))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Inodes;

    #[test]
    fn numbers_are_stable_follow_renames_and_are_never_reused() {
        let mut inodes = Inodes::new();
        assert_eq!(inodes.ino(""), Inodes::ROOT);
        let a = inodes.ino("a");
        let inner = inodes.ino("a/b");
        assert_eq!(inodes.ino("a"), a);
        // A sibling whose name starts with the same text is not below it.
        let ab = inodes.ino("ab");
        inodes.rename("a", "c");
        assert_eq!(inodes.path(a), Some("c"));
        assert_eq!(inodes.path(inner), Some("c/b"));
        assert_eq!(inodes.path(ab), Some("ab"));
        inodes.forget("c");
        assert_eq!(inodes.path(a), None);
        assert_eq!(inodes.path(inner), None);
        let again = inodes.ino("c");
        assert!(again != a && again != inner, "a number was reused");
        // A rename over an existing name drops the old one's number.
        let x = inodes.ino("x");
        inodes.rename("ab", "x");
        assert_eq!(inodes.path(x), None);
        assert_eq!(inodes.path(ab), Some("x"));
    }
}
