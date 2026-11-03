//! The bond store's file layer over the kernel's file syscalls
//! (`bthost::store::Fs`): the same write-then-rename `keyd` uses, so a power
//! cut leaves the old file or the new one, never half of either.

use alloc::vec::Vec;
use bthost::store::{Fs, Paths};
use user::files;

/// Where the bond store lives (`fhs::state::BTD_*`).
pub(super) const PATHS: Paths = Paths {
    dirs: &[fhs::state::BTD_DIR],
    bonds: fhs::state::BTD_BONDS,
    new: fhs::state::BTD_BONDS_NEW,
    bad: fhs::state::BTD_BONDS_BAD,
};

pub(super) struct Disk;

impl Fs for Disk {
    fn read(&mut self, path: &str) -> Result<Vec<u8>, i64> {
        files::read_up_to(path, btbond::MAX_FILE)
    }

    fn make_dir(&mut self, dir: &str) -> Result<(), i64> {
        match files::stat(dir) {
            Ok(_) => Ok(()),
            Err(_) => files::mkdir(dir).and_then(|()| files::chmod(dir, 0o700)),
        }
    }

    fn write_secret(&mut self, path: &str, bytes: &[u8]) -> Result<(), i64> {
        files::write_large(path, bytes).and_then(|()| files::chmod(path, 0o600))
    }

    fn sync(&mut self, path: &str) -> Result<(), i64> {
        files::fsync(path)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), i64> {
        files::rename(from, to)
    }

    fn remove(&mut self, path: &str) -> Result<(), i64> {
        files::remove(path)
    }
}
