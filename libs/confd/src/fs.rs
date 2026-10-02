//! Crash-safe persistence over a tiny filesystem abstraction.
//!
//! `confd` owns one directory; the only operations it needs are read, write,
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

/// Name a store file is moved to once its entries were merged into a better
/// location, so a later start cannot resurrect values deleted since.
pub const MIGRATED_FILE: &str = "store.migrated";

/// The file operations [`persist`] and [`load`] are built from.
///
/// Real implementations wrap a directory handle from `confd`; tests wrap an
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
///
/// The directory entry left by the rename is not itself fsynced (the trait
/// has no directory operation), so a power loss immediately after a rename
/// can still revert to the previous store — which is exactly the old-or-new
/// guarantee, not a torn one.
pub fn persist<F: StoreFs>(fs: &mut F, store: &Store) -> Result<(), F::Error> {
    let bytes = encode(store);
    fs.write_file(TMP_FILE, &bytes)?;
    fs.fsync(TMP_FILE)?;
    fs.rename(TMP_FILE, STORE_FILE)
}

/// Loads the committed store, recovering from interrupted writes.
///
/// A leftover [`TMP_FILE`] next to a store is an interrupted [`persist`] and
/// is removed. A [`TMP_FILE`] with *no* store is the newest commit cut off
/// inside its rename: [`persist`] fsyncs the temporary file first, and a
/// filesystem may drop the destination entry before linking the new one
/// (ext2 does). If it decodes (the CRC proves it complete) it is promoted to
/// the store; otherwise it is removed. A missing store is an empty store. A
/// store that fails to decode is moved to [`CORRUPT_FILE`] (replacing any
/// older one) and an empty store is returned, matching the plan's "start
/// empty and keep the bad file" behaviour.
pub fn load<F: StoreFs>(fs: &mut F) -> Result<Store, F::Error> {
    match fs.read_file(STORE_FILE)? {
        None => recover_tmp(fs),
        Some(bytes) => {
            fs.remove(TMP_FILE)?;
            match decode(&bytes) {
                Ok(store) => Ok(store),
                Err(_) => {
                    fs.rename(STORE_FILE, CORRUPT_FILE)?;
                    Ok(Store::new())
                }
            }
        }
    }
}

/// Reads the store in `fs` without changing anything there: for a seed source
/// that must never be written (`/data/confd`). A missing store falls back to
/// a complete [`TMP_FILE`]; one that does not decode contributes nothing, and
/// is left where it is.
pub fn load_read_only<F: StoreFs>(fs: &mut F) -> Result<Store, F::Error> {
    let bytes = match fs.read_file(STORE_FILE)? {
        Some(bytes) => Some(bytes),
        None => fs.read_file(TMP_FILE)?,
    };
    Ok(bytes
        .and_then(|bytes| decode(&bytes).ok())
        .unwrap_or_default())
}

/// Promote a complete [`TMP_FILE`] left without a store (see [`load`]).
fn recover_tmp<F: StoreFs>(fs: &mut F) -> Result<Store, F::Error> {
    if let Some(bytes) = fs.read_file(TMP_FILE)? {
        if let Ok(store) = decode(&bytes) {
            fs.rename(TMP_FILE, STORE_FILE)?;
            return Ok(store);
        }
        fs.remove(TMP_FILE)?;
    }
    Ok(Store::new())
}

/// Marks the store in `fs` as merged elsewhere (best effort: a failure only
/// means the merge is repeated, and it never overwrites a newer value).
pub fn retire<F: StoreFs>(fs: &mut F) {
    let _ = fs.rename(STORE_FILE, MIGRATED_FILE);
}
