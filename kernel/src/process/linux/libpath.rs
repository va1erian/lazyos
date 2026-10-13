//! Where Linux programs look for shared objects: `/lib/<name>`,
//! `/usr/lib/<name>`, `/usr/local/lib/<name>` and `/lib64/<name>` (musl's
//! default `ld.so` search path, glibc's `PT_INTERP` directories).
//!
//! LazyOS keeps them in one directory, [`fhs::SYSTEM_LIB`], written by the
//! image build. A name in one of the conventional directories is served from
//! there when no file of that name exists, so the interpreter path a prebuilt
//! binary carries (`/lib/ld-musl-x86_64.so.1`) and a `dlopen("libfoo.so")`
//! search both find it without links (the filesystem stores none yet).
//!
//! Names are used as typed: the volume is case-sensitive and nothing here
//! folds case.

use alloc::format;
use alloc::string::String;

use crate::fs::vfs::{FileKind, FsError, Id};
use crate::process::image::VfsFile;

/// The conventional library directories, with the trailing slash.
const ALIAS_DIRS: [&str; 4] = ["/lib/", "/usr/lib/", "/usr/local/lib/", "/lib64/"];

/// The `/system/lib` path a name in a conventional library directory stands
/// for: one path component, so `/lib/../etc/passwd` is nothing.
pub(super) fn system_lib_path(path: &str) -> Option<String> {
    let name = ALIAS_DIRS.iter().find_map(|dir| path.strip_prefix(dir))?;
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return None;
    }
    Some(format!("{}/{name}", fhs::SYSTEM_LIB))
}

/// The file `path` stands for when no file of that name exists but its
/// `/system/lib` counterpart does.
pub(super) fn alias(path: &str) -> Option<String> {
    let target = system_lib_path(path)?;
    let id = Id::current();
    match crate::fs::abi_stat(id, path) {
        Err(FsError::NotFound) => {}
        _ => return None,
    }
    crate::fs::abi_stat(id, &target)
        .ok()
        .filter(|meta| meta.kind == FileKind::File)
        .map(|_| target)
}

/// Open the program interpreter a `PT_INTERP` names, as the caller may read
/// it: the file at `path` exactly as stored, else its `/system/lib`
/// counterpart.
pub(crate) fn open_interpreter(path: &str) -> Result<VfsFile, FsError> {
    let id = Id::current();
    match alias(path) {
        Some(target) => VfsFile::abi(id, &target),
        None => VfsFile::abi(id, path),
    }
}
