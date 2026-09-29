#![forbid(unsafe_code)]

//! Path helpers that assume nothing about drive letters or separators: a
//! LazyOS port with `/`-separated paths works through the same functions.

use std::path::Path;

/// The title for a folder window: its final component, or the display form of a
/// filesystem root (which has no final component), e.g. `/` or `C:\`.
///
/// A trailing separator is ignored for a normal folder (`a/b/` is `b`); a
/// non-UTF-8 name is shown lossily.
pub fn title(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// Whether `path` is a filesystem root, detected by having no parent rather
/// than by naming a separator. A root (and an empty path) has none.
pub fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

/// Whether `path` is `ancestor` itself or lies below it, compared by path
/// components (so `a/b` is within `a` but `ab` is not).
pub fn is_within(path: &Path, ancestor: &Path) -> bool {
    path.starts_with(ancestor)
}

/// Why `path` may not be deleted, or `None` when it may.
///
/// A filesystem root has no parent and is always refused; every other path is
/// allowed. This is the guard the delete action consults before touching disk.
pub fn deletion_refused(path: &Path) -> Option<&'static str> {
    is_root(path).then_some("a filesystem root cannot be deleted")
}
