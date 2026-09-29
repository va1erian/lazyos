//! Crash-safe persistence over a tiny filesystem abstraction.
//!
//! `regd` owns one directory; the only operations it needs are read, write,
//! fsync, atomic rename and remove, so the service binds [`StoreFs`] to its
//! ext2 handle and tests bind it to an in-memory map.

use alloc::vec::Vec;

use crate::codec::{decode, encode};
use crate::store::Store;

/// Name of the committed store file.
pub const STORE_FILE: &str = "store";
/// Name of the temporary file a write goes through.
pub const TMP_FILE: &str = "store.tmp";
/// Name a store file too corrupt to read is moved to, for inspection.
pub const CORRUPT_FILE: &str = "store.corrupt";

/// The file operations [`persist`] and [`load`] are built from.
///
/// Real implementations wrap a directory handle from `regd`; tests wrap an
/// in-memory map. The contract that makes crash safety possible is
/// [`StoreFs::rename`]: it replaces `to` atomically (in the same directory),
/// so an interrupted [`persist`] leaves `to` either untouched or completely
/// replaced, never a mixture. [`StoreFs::write_file`] may truncate and fail
/// mid-way, but [`persist`] only ever points it at the temporary file.
pub trait StoreFs {
    /// A filesystem-specific error. `persist` and `load` propagate it
    /// unchanged.
    type Error;

    /// Reads the whole file, or `Ok(None)` when it does not exist.
    fn read_file(&mut self, name: &str) -> Result<Option<Vec<u8>>, Self::Error>;

    /// Creates or truncates `name` and writes `data`.
    fn write_file(&mut self, name: &str, data: &[u8]) -> Result<(), Self::Error>;

    /// Flushes `name` to durable storage. [`persist`] calls this before the
    /// rename, so the new bytes survive a power loss that follows.
    fn fsync(&mut self, name: &str) -> Result<(), Self::Error>;

    /// Atomically replaces `to` with `from`, both in the same directory.
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Self::Error>;

    /// Removes `name`; a missing file is not an error.
    fn remove(&mut self, name: &str) -> Result<(), Self::Error>;
}

/// Writes `store` as the new committed store.
///
/// The new image is written to [`TMP_FILE`], fsynced, then renamed over
/// [`STORE_FILE`]. A crash between any two steps leaves the old file in
/// place; the rename itself is atomic, so the visible store is always one
/// complete image.
pub fn persist<F: StoreFs>(fs: &mut F, store: &Store) -> Result<(), F::Error> {
    let bytes = encode(store);
    fs.write_file(TMP_FILE, &bytes)?;
    fs.fsync(TMP_FILE)?;
    fs.rename(TMP_FILE, STORE_FILE)
}

/// Loads the committed store, recovering from interrupted writes.
///
/// A leftover [`TMP_FILE`] from an interrupted [`persist`] is removed first.
/// A missing store is an empty store. A store that fails to decode is moved
/// to [`CORRUPT_FILE`] (replacing any older one) and an empty store is
/// returned, matching the plan's "start empty and keep the bad file"
/// behaviour.
pub fn load<F: StoreFs>(fs: &mut F) -> Result<Store, F::Error> {
    fs.remove(TMP_FILE)?;
    match fs.read_file(STORE_FILE)? {
        None => Ok(Store::new()),
        Some(bytes) => match decode(&bytes) {
            Ok(store) => Ok(store),
            Err(_) => {
                fs.rename(STORE_FILE, CORRUPT_FILE)?;
                Ok(Store::new())
            }
        },
    }
}
