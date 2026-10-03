//! What every program start shares: task-name interning and the exec
//! permission gate (`spawnv`, the kernel's `execve` of a native program).

use super::creds::*;
use super::*;

/// Distinct spellings [`intern_service_name`] will leak before it falls back to
/// [`OVERFLOW_NAME`]; a real manifest has a few dozen services.
pub(super) const MAX_INTERNED_NAMES: usize = 64;
/// The task name given to programs whose spelling arrives after the intern
/// table is full.
pub(super) const OVERFLOW_NAME: &str = "service";

/// Intern the task name of the program at `path` (its basename, exactly as
/// spelled) into a `&'static str` for [`task::spawn_child`].
///
/// `Task::name` is `&'static str`, but the name comes from the supervisor's
/// manifest at runtime. Leaking each *distinct* name once (bounded by the
/// manifest, not by restart count) is the smallest way to satisfy that type
/// without adding an allocation policy to the task table.
pub(super) fn intern_service_name(path: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.is_empty() {
        return OVERFLOW_NAME;
    }
    let mut names = NAMES.lock();
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    // Names are compared exactly: the root volume is ext2, which is
    // case-sensitive, so `a` and `A` are different programs and intern as two
    // names. Only the basename counts, so `/system/bin/keyd` and
    // `./keyd` share `keyd`, but a caller can still invent names that fail to
    // resolve. Past the cap every new name shares one generic name instead
    // of leaking another string.
    if names.len() >= MAX_INTERNED_NAMES {
        return OVERFLOW_NAME;
    }
    let leaked: &'static str = Box::leak(String::from(name).into_boxed_str());
    names.push(leaked);
    leaked
}

/// The gate every native-side program start passes before a byte of the
/// image is read, as the current task: the `noexec` mount check first, then
/// `EXECUTE` on the file (root needs an `x` bit too) and a regular file.
/// `linux` selects the `linux:` lookup, which lets a synthetic applet name
/// through ([`exec_perm::linux_spawn`]). `Err` is a negative errno: `EACCES`
/// when refused, `ENOENT` when a native program is missing.
pub(crate) fn check_exec(path: &str, linux: bool) -> Result<(), i64> {
    if fs::mount_flags(path).noexec {
        return Err(-EACCES);
    }
    let allowed = if linux {
        exec_perm::linux_spawn(path)
    } else {
        exec_perm::native(path)
    };
    match allowed {
        Ok(()) => Ok(()),
        Err(fs::vfs::FsError::NotFound) => Err(-ENOENT),
        Err(_) => Err(-EACCES),
    }
}
