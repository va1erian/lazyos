//! Lexical VFS path normalization.

use alloc::string::String;
use alloc::vec::Vec;

/// A normalized, absolute VFS path: `/` plus components with `.`/`..` already
/// folded. Symlinks are not followed (there are none); see the module docs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Path {
    /// Whether the raw input started with `/`. Relative paths resolve from the
    /// root too until per-process cwds exist.
    absolute: bool,
    pub(super) parts: Vec<String>,
}

impl Path {
    /// Fold a raw path into components: empty and `.` components drop, `..`
    /// pops (never above the root), and repeated slashes collapse. An empty
    /// input becomes the root.
    pub fn parse(raw: &str) -> Path {
        let absolute = raw.starts_with('/');
        let mut parts = Vec::new();
        for part in raw.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                name => parts.push(String::from(name)),
            }
        }
        Path { absolute, parts }
    }

    /// Whether the raw input was absolute. Both forms resolve from the root
    /// today, so this is informational (and tested).
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn is_absolute(&self) -> bool {
        self.absolute
    }

    pub fn is_root(&self) -> bool {
        self.parts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// The final component, or `None` for the root.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub fn name(&self) -> Option<&str> {
        self.parts.last().map(String::as_str)
    }

    /// The containing directory (the root's parent is the root).
    pub fn parent(&self) -> Path {
        let len = self.parts.len().saturating_sub(1);
        Path {
            absolute: true,
            parts: self.parts[..len].to_vec(),
        }
    }

    /// Every proper ancestor directory, root first. The root itself has none,
    /// so a search-permission walk checks exactly the directories that lead to
    /// the node and never the node twice.
    pub fn ancestors(&self) -> Vec<Path> {
        if self.parts.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(self.parts.len());
        out.push(Path {
            absolute: true,
            parts: Vec::new(),
        });
        for depth in 1..self.parts.len() {
            out.push(Path {
                absolute: true,
                parts: self.parts[..depth].to_vec(),
            });
        }
        out
    }

    /// Whether `prefix` is this path or one of its ancestors.
    pub fn starts_with(&self, prefix: &Path) -> bool {
        prefix.parts.len() <= self.parts.len()
            && self.parts[..prefix.parts.len()] == prefix.parts[..]
    }

    /// The canonical string form: `/`, `/a`, `/a/b`, ...
    pub fn to_path_string(&self) -> String {
        let mut out = String::new();
        for part in &self.parts {
            out.push('/');
            out.push_str(part);
        }
        if out.is_empty() {
            out.push('/');
        }
        out
    }
}
