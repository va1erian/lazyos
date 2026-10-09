//! Open files kept between requests. The kernel never says when a program
//! closes a file, and each SMB `CREATE`/`CLOSE` pair is two round trips, so
//! a file read or written is kept open for the next request: at most
//! [`MAX_HANDLES`], the least recently used closed first, and any unused for
//! [`IDLE_TICKS`] closed by [`crate::SmbFs::idle`]. A handle never outlives
//! a rename or removal of its path (the daemon closes it first).

use alloc::string::String;
use alloc::vec::Vec;

use smbwire::msg::FileId;

/// Most files kept open at once.
pub const MAX_HANDLES: usize = 8;
/// Ticks (100 Hz) an unused handle stays open: 5 s.
pub const IDLE_TICKS: u64 = 500;

struct Handle {
    path: String,
    id: FileId,
    write: bool,
    used: u64,
}

#[derive(Default)]
pub struct Handles {
    open: Vec<Handle>,
}

/// Whether `path` is `top` or below it.
fn under(path: &str, top: &str) -> bool {
    top.is_empty()
        || path == top
        || (path.starts_with(top) && path.as_bytes().get(top.len()) == Some(&b'/'))
}

impl Handles {
    /// An open handle for `path` good enough for the access asked (a
    /// writable one also reads), marked used at `now`.
    pub fn find(&mut self, path: &str, write: bool, now: u64) -> Option<FileId> {
        let handle = self
            .open
            .iter_mut()
            .find(|h| h.path == path && (h.write || !write))?;
        handle.used = now;
        Some(handle.id)
    }

    /// Record a newly opened handle. The caller made room first
    /// ([`Handles::make_room`]).
    pub fn insert(&mut self, path: &str, id: FileId, write: bool, now: u64) {
        self.open.push(Handle {
            path: String::from(path),
            id,
            write,
            used: now,
        });
    }

    /// The handles of `path` and everything below it, taken out (to close).
    pub fn take_under(&mut self, path: &str) -> Vec<FileId> {
        self.take(|h| under(&h.path, path))
    }

    /// The handles of exactly `path`, taken out (a read-only one being
    /// replaced by a writable one).
    pub fn take_path(&mut self, path: &str) -> Vec<FileId> {
        self.take(|h| h.path == path)
    }

    /// The least recently used handle, taken out when the pool is full.
    pub fn make_room(&mut self) -> Option<FileId> {
        if self.open.len() < MAX_HANDLES {
            return None;
        }
        let oldest = (0..self.open.len()).min_by_key(|&i| self.open[i].used)?;
        Some(self.open.remove(oldest).id)
    }

    /// Handles unused for [`IDLE_TICKS`] at `now`, taken out.
    pub fn take_idle(&mut self, now: u64) -> Vec<FileId> {
        self.take(|h| now.saturating_sub(h.used) >= IDLE_TICKS)
    }

    /// Every writable handle (a flush makes their data durable).
    pub fn writable(&self) -> Vec<FileId> {
        self.open.iter().filter(|h| h.write).map(|h| h.id).collect()
    }

    pub fn count(&self) -> usize {
        self.open.len()
    }

    /// Forget every handle without closing it (its session is gone).
    pub fn clear(&mut self) {
        self.open.clear();
    }

    fn take(&mut self, mut which: impl FnMut(&Handle) -> bool) -> Vec<FileId> {
        let mut taken = Vec::new();
        self.open.retain(|h| {
            if which(h) {
                taken.push(h.id);
                false
            } else {
                true
            }
        });
        taken
    }
}
