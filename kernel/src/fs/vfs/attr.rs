//! Node attributes beyond the tree: timestamps, the filesystem clock, and the
//! one change set ([`SetAttr`]) every backend applies for `chmod`, `chown` and
//! `utimensat`. Who may ask for which change is decided in `setattr.rs`; a
//! backend only stores what it is handed.

/// The three POSIX timestamps of a node, in whole seconds since the Unix epoch
/// (`time_t`, so negative is before 1970). No backend keeps sub-second
/// precision: a revision-1 ext2 inode has no room for it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Times {
    /// Last access. Reads do not move it (every mount behaves as `noatime`);
    /// only `utimensat` and creation set it.
    pub atime: i64,
    /// Last change to the contents.
    pub mtime: i64,
    /// Last change to the contents *or* the attributes.
    pub ctime: i64,
}

impl Times {
    /// All three stamps at `time`, as a freshly created node carries them.
    pub const fn all(time: i64) -> Times {
        Times {
            atime: time,
            mtime: time,
            ctime: time,
        }
    }
}

/// The filesystem clock: seconds since boot from the 100 Hz PIT
/// (`arch::pic` programs that rate), until an RTC driver gives wall time.
/// Every backend stamps from here and `UTIME_NOW` resolves here, so a
/// `touch` and a write agree on what "now" is.
pub fn now() -> i64 {
    (crate::task::ticks() / 100) as i64
}

/// One attribute change as a backend applies it. The `Option`s are the
/// explicit set-mask: a `Some` field is in the set and overwrites the stored
/// value, a `None` field is left alone. A value can never be present without
/// being selected, or selected without a value, which a separate bit mask
/// beside the values would allow.
///
/// The VFS builds this after its permission rules ran (see
/// [`crate::fs::vfs::Vfs::setattr`]), so it is already authorized and already
/// carries the side effects POSIX asks for (the `ctime` bump, setuid/setgid
/// clearing). A backend must apply every field or fail without changing any.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct SetAttr {
    /// Permission bits (`0o7777`); the node's type bits never change.
    pub mode: Option<u16>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<i64>,
    pub mtime: Option<i64>,
    pub ctime: Option<i64>,
}

impl SetAttr {
    /// Whether nothing is selected, so a backend need not be called at all.
    pub fn is_empty(&self) -> bool {
        *self == SetAttr::default()
    }

    /// Only the timestamps of `times`: what copying a node between backends
    /// uses to carry its times along.
    pub fn times(times: Times) -> SetAttr {
        SetAttr {
            atime: Some(times.atime),
            mtime: Some(times.mtime),
            ctime: Some(times.ctime),
            ..SetAttr::default()
        }
    }

    /// Overwrite the selected timestamps in `times`; for backends that keep a
    /// [`Times`] as it is.
    pub fn apply_times(&self, times: &mut Times) {
        if let Some(atime) = self.atime {
            times.atime = atime;
        }
        if let Some(mtime) = self.mtime {
            times.mtime = mtime;
        }
        if let Some(ctime) = self.ctime {
            times.ctime = ctime;
        }
    }
}

/// A timestamp an `utimensat`-style call asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stamp {
    /// `UTIME_NOW` (or a `NULL` times array): the filesystem clock.
    Now,
    /// An explicit time, in seconds since the epoch.
    At(i64),
}

/// What a caller asks to change, before the permission rules turn it into a
/// [`SetAttr`]. One variant per syscall family, because each family has its
/// own rule for who may do it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AttrRequest {
    /// `chmod`: new permission bits (masked to `0o7777`).
    Mode(u16),
    /// `chown`: `None` is the `-1` that leaves an id alone.
    Owner { uid: Option<u32>, gid: Option<u32> },
    /// `utimensat`: `None` is `UTIME_OMIT`.
    Times {
        atime: Option<Stamp>,
        mtime: Option<Stamp>,
    },
}
