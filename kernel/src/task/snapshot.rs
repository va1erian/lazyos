//! Shared, copy-on-write file snapshots for `Fd::File` descriptors.
//!
//! `open` copies a file into the kernel heap once; `dup`/`fork` then share
//! that copy through the `Arc` and only the first writer detaches its own.
//! Every allocation here is fallible: the heap is small (16 MiB), so running
//! out must fail the one call, never abort the kernel.

use alloc::sync::Arc;
use alloc::vec::Vec;

/// Mirror a write of `data` at `offset` into `snapshot`, detaching it from
/// other holders first. Returns `false` (leaving the snapshot unchanged) on
/// arithmetic overflow or allocation failure.
///
/// The backing file already accepted this write and its size cap, so this
/// only keeps the descriptor's view current.
pub fn write_at(snapshot: &mut Arc<Vec<u8>>, offset: usize, data: &[u8]) -> bool {
    let Some(end) = offset.checked_add(data.len()) else {
        return false;
    };
    if Arc::get_mut(snapshot).is_none() {
        let mut own = Vec::new();
        if own.try_reserve_exact(snapshot.len().max(end)).is_err() {
            return false;
        }
        own.extend_from_slice(snapshot);
        *snapshot = Arc::new(own);
    }
    // INVARIANT: the branch above left this `Arc` uniquely owned.
    let bytes = Arc::get_mut(snapshot).expect("snapshot is unshared");
    if end > bytes.len() {
        if bytes.try_reserve_exact(end - bytes.len()).is_err() {
            return false;
        }
        bytes.resize(end, 0);
    }
    bytes[offset..end].copy_from_slice(data);
    true
}
