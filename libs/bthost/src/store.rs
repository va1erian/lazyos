//! The bond store's file layer (docs/bluetooth-plan.md decision 4), written
//! against a small [`Fs`] trait so the same code runs in `btd` (over
//! `user::files`) and in host tests (over memory, including a crash between
//! the write and the rename).
//!
//! * [`load`] reads the file before the controller is up. Absent is an empty
//!   store; a file `btbond` refuses (damaged, a layout it does not read) is
//!   moved aside to `bonds.bad` so the next save never destroys it; one that
//!   cannot be read at all is left alone.
//! * [`save`] writes the new bytes to `bonds.new` (0600), syncs, renames over
//!   `bonds`, and syncs again, as `keyd` does: after a crash the old file or
//!   the new one is there, never half of either.

use alloc::vec::Vec;

use btbond::BondStore;

/// `ENOENT`: the file is not there.
pub const ENOENT: i64 = 2;

/// Where the files live (`fhs::state::BTD_*`; tests use their own).
#[derive(Clone, Copy, Debug)]
pub struct Paths {
    /// The directories to create, outermost first (0700).
    pub dirs: &'static [&'static str],
    pub bonds: &'static str,
    pub new: &'static str,
    pub bad: &'static str,
}

/// The file operations the layer needs. Errors are errno values.
pub trait Fs {
    /// The whole file, or `Err(ENOENT)` when it is absent.
    fn read(&mut self, path: &str) -> Result<Vec<u8>, i64>;
    /// Create `dir` (0700) unless it exists.
    fn make_dir(&mut self, dir: &str) -> Result<(), i64>;
    /// Write `bytes` to `path`, replacing it, mode 0600.
    fn write_secret(&mut self, path: &str, bytes: &[u8]) -> Result<(), i64>;
    fn sync(&mut self, path: &str) -> Result<(), i64>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64>;
    fn remove(&mut self, path: &str) -> Result<(), i64>;
}

/// What [`load`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Loaded {
    /// A good file with this many bonds.
    Bonds(usize),
    /// No file yet.
    Missing,
    /// The file was refused and moved aside (or could not be, `moved` false).
    Damaged { moved: bool },
    /// The file exists but cannot be read (errno).
    Unreadable(i64),
}

/// Read the bond store; a store is always returned (empty on any trouble).
pub fn load(fs: &mut impl Fs, paths: &Paths) -> (BondStore, Loaded) {
    let bytes = match fs.read(paths.bonds) {
        Ok(bytes) => bytes,
        Err(ENOENT) => return (BondStore::new(), Loaded::Missing),
        Err(errno) => return (BondStore::new(), Loaded::Unreadable(errno)),
    };
    match BondStore::decode(&bytes) {
        Ok(store) => {
            let n = store.len();
            (store, Loaded::Bonds(n))
        }
        Err(_) => {
            // Keep the damaged file for whoever can use it; the next save
            // must not destroy it.
            let moved = fs.rename(paths.bonds, paths.bad).is_ok();
            (BondStore::new(), Loaded::Damaged { moved })
        }
    }
}

/// Which step of [`save`] failed, and the errno it gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SaveError {
    pub step: &'static str,
    pub errno: i64,
}

fn at(step: &'static str) -> impl Fn(i64) -> SaveError {
    move |errno| SaveError { step, errno }
}

/// Write the encoded store atomically. Removes the temporary file on failure.
pub fn save(fs: &mut impl Fs, paths: &Paths, bytes: &[u8]) -> Result<(), SaveError> {
    for dir in paths.dirs {
        fs.make_dir(dir).map_err(at("mkdir"))?;
    }
    let result = fs
        .write_secret(paths.new, bytes)
        .map_err(at("write"))
        .and_then(|()| fs.sync(paths.new).map_err(at("sync-new")))
        .and_then(|()| fs.rename(paths.new, paths.bonds).map_err(at("rename")))
        .and_then(|()| fs.sync(paths.bonds).map_err(at("sync")));
    if result.is_err() {
        let _ = fs.remove(paths.new);
    }
    result
}
