//! Service core: the uid-checked operations `confd` serves, with
//! persist-before-swap semantics.
//!
//! This layer is deliberately free of Messenger and syscalls so the `confd`
//! binary and the kernel test suite drive the same commit logic. The store
//! itself ([`crate::Store`]) already validates paths, limits and access; what
//! this module adds is the ordering the review of #267 requires:
//!
//! * a mutation is applied to a **clone** of the committed store;
//! * the clone is persisted; only if that succeeds is it swapped in;
//! * only then is the change announced.
//!
//! A persist failure therefore leaves the live store byte-for-byte unchanged
//! and publishes nothing, which is what makes `CONFD_IO` safe to retry.

use alloc::vec::Vec;

use crate::fs::{load, load_read_only, persist, retire, StoreFs};
use crate::store::{Caller, Change, Error, Store};
use crate::value::Value;

/// Why a service operation failed. The variants map one-to-one to the
/// `CONFD_*` codes the wire protocol carries.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ServiceError {
    /// The path is absent.
    NotFound,
    /// The path is not valid.
    BadPath,
    /// The value or store exceeds a size limit.
    TooLarge,
    /// The caller may not access the path.
    Denied,
    /// The backing store could not be read or written.
    Io,
}

impl ServiceError {
    /// A short, human-readable explanation (friendly-errors convention).
    pub const fn message(self) -> &'static str {
        match self {
            ServiceError::NotFound => "no value is stored at that path",
            ServiceError::BadPath => "that is not a valid confd path",
            ServiceError::TooLarge => "the value or store exceeds a confd size limit",
            ServiceError::Denied => "the caller may not access that path",
            ServiceError::Io => "the confd store could not be read or written",
        }
    }
}

impl core::fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str(self.message())
    }
}

/// Map a store rejection onto the service surface (the only conversion: a
/// store never produces `NotFound` or `Io`).
fn from_store(error: Error) -> ServiceError {
    match error {
        Error::BadPath => ServiceError::BadPath,
        Error::TooLarge => ServiceError::TooLarge,
        Error::Denied => ServiceError::Denied,
    }
}

/// A sink for committed change notifications.
///
/// Publishing is best-effort and has no error channel on purpose: a committed
/// write must not be reported as failed just because a subscriber could not be
/// reached (the plan calls change topics best-effort).
pub trait ChangeSink {
    /// A path was committed; `deleted` is `true` for a `Delete` and `false`
    /// for a `Set`. The value is deliberately not passed.
    fn changed(&mut self, path: &str, deleted: bool);
}

/// Whether changes to `path` may be announced on a topic.
///
/// The kernel topic policy cannot express "`user/<uid>` is readable only by
/// that uid", so announcing user paths would leak their existence and change
/// timing to any subscriber. Until the policy hook grows a per-uid rule, only
/// the world-readable `sys/` subtree is announced (the approved fallback in
/// issue #260's follow-ups).
pub fn announceable(path: &str) -> bool {
    path == "sys" || starts_with_sys(path)
}

/// `str::starts_with("sys/")` without the trait machinery.
fn starts_with_sys(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() < 4 {
        return false;
    }
    bytes[0] == b's' && bytes[1] == b'y' && bytes[2] == b's' && bytes[3] == b'/'
}

/// What merging another store into the live one did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Migration {
    /// Entries copied because the destination had no value at that path.
    pub added: usize,
    /// Entries left behind because they exceed a limit; when non-zero the
    /// source store is kept so nothing is lost.
    pub skipped: usize,
}

/// The registry state: the committed store, its backing filesystem, and the
/// change sink.
pub struct Confd<F: StoreFs, S: ChangeSink> {
    store: Store,
    fs: F,
    sink: S,
}

impl<F: StoreFs, S: ChangeSink> Confd<F, S> {
    /// Load the committed store from `fs` and bind it to `sink`.
    ///
    /// A corrupt store is recovered by [`load`] (moved aside, empty store); a
    /// real filesystem failure is returned so the service can refuse to run
    /// rather than serve an untrustworthy empty tree.
    pub fn load(mut fs: F, sink: S) -> Result<Self, ServiceError> {
        let store = load(&mut fs).map_err(|_| ServiceError::Io)?;
        Ok(Self { store, fs, sink })
    }

    /// The committed store (diagnostics and tests).
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// The backing filesystem (tests build a fresh instance from a clone).
    pub fn fs(&self) -> &F {
        &self.fs
    }

    /// The change sink (diagnostics and tests).
    pub fn sink(&self) -> &S {
        &self.sink
    }

    /// The change sink, mutably (a service publishes its own heartbeat and,
    /// indirectly, its change topics through it).
    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }

    /// Moves the service onto `new_fs` (a better store that just became
    /// usable), carrying the live settings along.
    ///
    /// The destination's own values win: only paths it lacks are seeded from
    /// the live store, so a populated `/data` is never clobbered. The merged
    /// store is persisted to `new_fs` *before* the switch; on any error the
    /// service keeps its old backing store and state. Paths whose value
    /// changes for readers are announced. The old store file is retired
    /// (renamed to [`crate::MIGRATED_FILE`]) once fully merged.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Io`] when `new_fs` cannot be read or written.
    pub fn rebind(&mut self, mut new_fs: F) -> Result<Migration, ServiceError> {
        let mut merged = load(&mut new_fs).map_err(|_| ServiceError::Io)?;
        let before = merged.clone();
        let (added, skipped) = merged.merge_missing(&self.store);
        if merged != before {
            persist(&mut new_fs, &merged).map_err(|_| ServiceError::Io)?;
        }
        let old_store = core::mem::replace(&mut self.store, merged);
        let mut old_fs = core::mem::replace(&mut self.fs, new_fs);
        if skipped == 0 {
            retire(&mut old_fs);
        }
        self.announce_differences(&old_store);
        Ok(Migration {
            added: added.len(),
            skipped,
        })
    }

    /// Merges the store found in `source` (a lower-ranked location that may
    /// hold settings written before the current one was usable) into the live
    /// store without overwriting existing values, persists, and retires
    /// `source` once nothing was skipped. An empty or missing source is a
    /// no-op.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Io`] when `source` cannot be read or the merged store
    /// cannot be persisted; the live store is then unchanged.
    pub fn absorb<G: StoreFs>(&mut self, source: &mut G) -> Result<Migration, ServiceError> {
        let other = load(source).map_err(|_| ServiceError::Io)?;
        if other.is_empty() {
            return Ok(Migration::default());
        }
        let mut draft = self.store.clone();
        let (added, skipped) = draft.merge_missing(&other);
        if !added.is_empty() {
            persist(&mut self.fs, &draft).map_err(|_| ServiceError::Io)?;
            let old_store = core::mem::replace(&mut self.store, draft);
            self.announce_differences(&old_store);
        }
        if skipped == 0 {
            retire(source);
        }
        Ok(Migration {
            added: added.len(),
            skipped,
        })
    }

    /// Seeds the live store from the legacy store (`/data/confd`) **once**.
    ///
    /// The marker [`crate::dir::SEEDED_MARKER_FILE`] in the live store's
    /// directory records that it happened; with the marker present this is a
    /// no-op returning `Ok(None)`, so a setting deleted after the migration
    /// does not come back. Otherwise the entries of `legacy` (if any: `None`
    /// is an absent legacy store) the live store lacks are merged in and
    /// persisted, then the marker is written. `legacy` itself is never
    /// written: it is read-only seed data until F7 removes it.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Io`] when the marker cannot be read or written, the
    /// legacy store cannot be read or the merged store cannot be persisted;
    /// no marker is written then, so the next start retries.
    pub fn seed_once<G: StoreFs>(
        &mut self,
        legacy: Option<&mut G>,
    ) -> Result<Option<Migration>, ServiceError> {
        let marker = crate::dir::SEEDED_MARKER_FILE;
        if self
            .fs
            .read_file(marker)
            .map_err(|_| ServiceError::Io)?
            .is_some()
        {
            return Ok(None);
        }
        let mut report = Migration::default();
        if let Some(source) = legacy {
            let other = load_read_only(source).map_err(|_| ServiceError::Io)?;
            let mut draft = self.store.clone();
            let (added, skipped) = draft.merge_missing(&other);
            if !added.is_empty() {
                persist(&mut self.fs, &draft).map_err(|_| ServiceError::Io)?;
                let old_store = core::mem::replace(&mut self.store, draft);
                self.announce_differences(&old_store);
            }
            report = Migration {
                added: added.len(),
                skipped,
            };
        }
        self.fs
            .write_file(marker, b"seeded\n")
            .and_then(|()| self.fs.fsync(marker))
            .map_err(|_| ServiceError::Io)?;
        Ok(Some(report))
    }

    /// Announce every announceable path whose value differs from `old`
    /// (best effort).
    fn announce_differences(&mut self, old: &Store) {
        let root = Caller { uid: 0 };
        let changed: Vec<&str> = self
            .store
            .iter_raw()
            .filter(|(path, value)| old.get(path, root).ok().flatten() != Some(*value))
            .map(|(path, _)| path)
            .filter(|path| announceable(path))
            .collect();
        for path in changed {
            self.sink.changed(path, false);
        }
    }

    /// Reads the value at `path`, or `None` when it is absent.
    ///
    /// # Errors
    ///
    /// [`ServiceError::BadPath`] or [`ServiceError::Denied`].
    pub fn get(&self, path: &str, caller: Caller) -> Result<Option<&Value>, ServiceError> {
        self.store.get(path, caller).map_err(from_store)
    }

    /// Creates or overwrites `path`, persisting before the value is visible.
    ///
    /// # Errors
    ///
    /// [`ServiceError::BadPath`], [`ServiceError::Denied`],
    /// [`ServiceError::TooLarge`], or [`ServiceError::Io`]. On any error the
    /// committed store and the published state are unchanged.
    pub fn set(&mut self, path: &str, value: Value, caller: Caller) -> Result<(), ServiceError> {
        let mut draft = self.store.clone();
        let change = draft.set(path, value, caller).map_err(from_store)?;
        self.commit(draft, change)
    }

    /// Removes `path`. Deleting an absent path is a no-op (success).
    ///
    /// # Errors
    ///
    /// [`ServiceError::BadPath`], [`ServiceError::Denied`], or
    /// [`ServiceError::Io`]. On an error the committed store is unchanged.
    pub fn delete(&mut self, path: &str, caller: Caller) -> Result<(), ServiceError> {
        let mut draft = self.store.clone();
        let Some(change) = draft.delete(path, caller).map_err(from_store)? else {
            // Absent: nothing changed, so do not rewrite or announce.
            return Ok(());
        };
        self.commit(draft, change)
    }

    /// Lists the paths under `prefix` the caller may read.
    ///
    /// # Errors
    ///
    /// [`ServiceError::BadPath`] when a non-empty prefix is invalid.
    pub fn list(&self, prefix: &str, caller: Caller) -> Result<Vec<&str>, ServiceError> {
        self.store.list(prefix, caller).map_err(from_store)
    }

    /// Persist `draft`, swap it in on success, then announce the change.
    ///
    /// The announced path is [`announceable`]; a `user/` change is committed
    /// but never published (see [`announceable`]). A dropped sink call is
    /// best-effort and never fails the operation.
    fn commit(&mut self, draft: Store, change: Change) -> Result<(), ServiceError> {
        if persist(&mut self.fs, &draft).is_err() {
            // `self.store` was never touched; the draft, and any temporary
            // file `persist` left behind, are discarded by `load` on the next
            // start.
            return Err(ServiceError::Io);
        }
        self.store = draft;
        if announceable(&change.path) {
            self.sink.changed(&change.path, change.new.is_none());
        }
        Ok(())
    }
}

impl<F: StoreFs, S: ChangeSink> core::fmt::Debug for Confd<F, S> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Confd")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}
