//! Where `confd` keeps its store: the VFS binding, directory selection, and
//! the migration of settings into a better location.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use confd::{dir, ChangeSink, Confd, StoreFs};
use user::files::{self, Kind};
use user::sys;

/// `ENOENT`, spelled out because `files` reports raw errnos.
const ENOENT: i64 = 2;

/// A [`StoreFs`] binding the store files to one VFS directory.
///
/// The names are the `libs/confd` constants (`store`, `store.tmp`,
/// `store.corrupt`); this type only prefixes the directory.
pub struct VfsStoreFs {
    dir: String,
}

impl VfsStoreFs {
    pub fn new(dir: &str) -> VfsStoreFs {
        VfsStoreFs {
            dir: String::from(dir),
        }
    }

    /// The absolute path of one store file.
    fn path(&self, name: &str) -> String {
        let mut path = self.dir.clone();
        path.push('/');
        path.push_str(name);
        path
    }
}

impl StoreFs for VfsStoreFs {
    type Error = i64;

    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, i64> {
        match files::read_all(&self.path(name)) {
            Ok(data) => Ok(Some(data)),
            Err(errno) if errno == ENOENT => Ok(None),
            Err(errno) => Err(errno),
        }
    }

    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), i64> {
        // `write_file` creates-or-replaces, which is all `persist` needs; it
        // only ever points this at `store.tmp`.
        files::write_file(&self.path(name), data)
    }

    fn fsync(&mut self, name: &str) -> Result<(), i64> {
        files::fsync(&self.path(name))
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        files::rename(&self.path(from), &self.path(to))
    }

    fn remove(&mut self, name: &str) -> Result<(), i64> {
        match files::remove(&self.path(name)) {
            Ok(()) => Ok(()),
            // A missing file is not an error (the trait contract).
            Err(errno) if errno == ENOENT => Ok(()),
            Err(errno) => Err(errno),
        }
    }
}

/// Merge the stores left in locations ranked below `chosen` into the live
/// one. Failures are logged and retried on the next start: the live store is
/// never changed by a failed merge.
pub fn seed_from_lower<S: ChangeSink>(service: &mut Confd<VfsStoreFs, S>, chosen: &str) {
    for &source_dir in dir::seed_sources(chosen) {
        // A location that does not exist holds nothing to merge (and probing
        // a read-only one would only log noise).
        if !matches!(files::stat(source_dir), Ok((_, Kind::Dir))) {
            continue;
        }
        let mut source = VfsStoreFs::new(source_dir);
        match service.absorb(&mut source) {
            Ok(report) if report.added > 0 || report.skipped > 0 => {
                sys::write_str(&format!(
                    "CONFD:SEED from={source_dir} added={} skipped={}
",
                    report.added, report.skipped
                ));
            }
            Ok(_) => {}
            Err(error) => sys::write_str(&format!(
                "confd: cannot seed from {source_dir}: {}
",
                error.message()
            )),
        }
    }
}

/// If `/data/confd` is now usable, move the running service onto it (merging
/// the live settings in without clobbering existing values). Returns `true`
/// once the service is bound to the preferred store.
pub fn try_upgrade<S: ChangeSink>(service: &mut Confd<VfsStoreFs, S>) -> bool {
    if check_dir(dir::PREFERRED_DIR).is_err() {
        return false;
    }
    match service.rebind(VfsStoreFs::new(dir::PREFERRED_DIR)) {
        Ok(report) => {
            sys::write_str(&format!(
                "CONFD:SEED to={} added={} skipped={}
",
                dir::PREFERRED_DIR,
                report.added,
                report.skipped
            ));
            true
        }
        Err(error) => {
            sys::write_str(&format!(
                "confd: cannot move the store to {}: {}
",
                dir::PREFERRED_DIR,
                error.message()
            ));
            false
        }
    }
}

/// The store directory and whether it is persistent.
///
/// A persistent candidate is only accepted if it can be created (or already
/// is a directory) *and* a probe write succeeds. Otherwise `/tmp/confd`
/// (ramfs) is used and the service reports degraded.
pub fn pick_dir() -> (String, bool) {
    let choice = dir::choose(&dir::PERSISTENT_DIRS, |d| match check_dir(d) {
        Ok(()) => true,
        Err(why) => {
            // Say why a persistent location was passed over, so a silent
            // fallback to ramfs is diagnosable from the serial log.
            sys::write_str(&format!(
                "confd: {d} not usable: {why}
"
            ));
            false
        }
    });
    if !choice.persistent {
        if let Err(why) = ensure_dir(choice.dir) {
            sys::write_str(&format!(
                "confd: warning: {}: {why}
",
                choice.dir
            ));
        }
    }
    (String::from(choice.dir), choice.persistent)
}

/// Whether `dir` can hold the store: it exists (or can be created) and a probe
/// file can be written there.
fn check_dir(dir: &str) -> Result<(), String> {
    ensure_dir(dir)?;
    probe_writable(dir)
}

/// Succeeds when `path` is a directory, creating it when absent.
fn ensure_dir(path: &str) -> Result<(), String> {
    match files::stat(path) {
        Ok((_, Kind::Dir)) => Ok(()),
        Ok(_) => Err(String::from("exists but is not a directory")),
        Err(errno) if errno == ENOENT => {
            files::mkdir(path).map_err(|errno| format!("mkdir failed (errno {errno})"))
        }
        Err(errno) => Err(format!("stat failed (errno {errno})")),
    }
}

/// Succeeds when a file can be written and removed under `dir`.
fn probe_writable(dir: &str) -> Result<(), String> {
    let mut probe = String::from(dir);
    probe.push_str("/.probe");
    files::write_file(&probe, b"ok")
        .map_err(|errno| format!("probe write failed (errno {errno})"))?;
    let _ = files::remove(&probe);
    Ok(())
}
