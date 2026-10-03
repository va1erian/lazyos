//! The `/etc` entries a Linux program reads that are backed by real LazyOS
//! files (docs/tls-plan.md §5.1, §5.2):
//!
//! | Linux path | Backed by | Written by |
//! |---|---|---|
//! | `/etc/resolv.conf` | [`fhs::state::RESOLV_CONF`] | `netd`, once it has resolvers |
//! | `/etc/hosts` | [`fhs::etc::HOSTS`] | the image build |
//! | `/etc/ssl/certs/ca-certificates.crt` | [`fhs::etc::CA_BUNDLE`] | the image build |
//!
//! plus the synthetic directories `/etc/ssl` and `/etc/ssl/certs` that hold
//! the bundle. Like the rest of [`super::etcfs`] they are read-only and read
//! afresh on every open, so a lease renewal that changes the resolvers is seen
//! by the next `getaddrinfo`.
//!
//! The backing files are read through the *native* mount table with the
//! kernel's identity: they are the files `netd` and the image wrote, never a
//! copy a Linux program made in the ABI's copy-up overlay. Everything here is
//! public information (resolver addresses, host names, public certificates),
//! but it decides where every Linux program's lookups go and which servers it
//! trusts, so a backing file is served only when it and its directory belong
//! to a trusted writer and nobody else can write them: `/transient` is a
//! world-writable scratch volume, and a `resolv.conf` some other user planted
//! there before `netd` ran must not count.
//!
//! A file that is missing (or untrusted) is a missing entry (`ENOENT`, as on
//! Linux before the network is up), except `/etc/hosts`, which falls back to
//! `localhost` so the loopback name resolves on any image.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, Id, Meta};

/// Inode numbers of these entries, clear of [`super::etcfs`]'s and `/proc`'s.
const BASE_INO: u64 = 60;

/// The largest backing file served. The CA bundle is about 200 KiB; anything
/// far larger is not a file this module should copy into every opener.
pub(super) const MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Who may have written a backing file: root, plus `_netd` for the resolver
/// configuration on `/transient`.
const ROOT_ONLY: &[u32] = &[0];
const ROOT_OR_NETD: &[u32] = &[0, netpolicy::NETD_UID];

/// One backed entry: its Linux path, the LazyOS file behind it and the uids
/// trusted to own that file and its directory.
struct Backed {
    linux: &'static str,
    file: &'static str,
    writers: &'static [u32],
}

const BACKED: &[Backed] = &[
    Backed {
        linux: fhs::etc::LINUX_RESOLV_CONF,
        file: fhs::state::RESOLV_CONF,
        writers: ROOT_OR_NETD,
    },
    Backed {
        linux: fhs::etc::LINUX_HOSTS,
        file: fhs::etc::HOSTS,
        writers: ROOT_ONLY,
    },
    Backed {
        linux: fhs::etc::LINUX_CA_BUNDLE,
        file: fhs::etc::CA_BUNDLE,
        writers: ROOT_ONLY,
    },
];

/// The synthetic directories, each listing what sits directly below it.
const DIRS: &[&str] = &[fhs::etc::LINUX_SSL, fhs::etc::LINUX_SSL_CERTS];

/// `/etc/hosts` when the image has no (trusted) hosts file.
const DEFAULT_HOSTS: &str = "127.0.0.1\tlocalhost lazyos\n::1\tlocalhost\n";

/// Whether `path` is one of the synthetic directories below `/etc`.
pub(super) fn is_dir(path: &str) -> bool {
    DIRS.contains(&path)
}

fn entry(path: &str) -> Option<(usize, &'static Backed)> {
    BACKED
        .iter()
        .enumerate()
        .find(|(_, backed)| backed.linux == path)
}

/// Whether `meta` belongs to one of `writers` and nobody else may write it.
fn trusted(meta: &Meta, writers: &[u32]) -> bool {
    writers.contains(&meta.uid) && meta.mode & 0o022 == 0
}

/// The backing file's size, if it exists, is a regular file small enough to
/// serve, and it and its directory are [`trusted`].
fn backing_size(backed: &Backed) -> Option<u64> {
    let stat = |path: &str| crate::fs::vfs_stat(Id::ROOT, path).ok();
    let parent = backed.file.rsplit_once('/').map(|(dir, _)| dir)?;
    let dir = stat(parent)?;
    let file = stat(backed.file)?;
    let ok = dir.kind == FileKind::Dir
        && trusted(&dir, backed.writers)
        && file.kind == FileKind::File
        && trusted(&file, backed.writers)
        && file.size <= MAX_BYTES;
    ok.then_some(file.size)
}

/// The backing file's bytes, under the same conditions as [`backing_size`].
fn read_backing(backed: &Backed) -> Option<Vec<u8>> {
    backing_size(backed)?;
    let bytes = crate::fs::vfs_read(Id::ROOT, backed.file).ok()?;
    // The file may have grown between the stat and the read.
    (bytes.len() as u64 <= MAX_BYTES).then_some(bytes)
}

/// The bytes of the backed entry at `path`: its file's current contents,
/// `None` when `path` is not one or its file is missing or untrusted.
pub(super) fn contents(path: &str) -> Option<Vec<u8>> {
    let (_, backed) = entry(path)?;
    read_backing(backed).or_else(|| {
        (path == fhs::etc::LINUX_HOSTS).then(|| Vec::from(DEFAULT_HOSTS.as_bytes()))
    })
}

/// Metadata for a backed entry (world-readable, read-only, its file's size)
/// or one of the synthetic directories; `None` for anything else, including
/// an entry whose file is missing or untrusted.
pub(super) fn meta(path: &str) -> Option<Meta> {
    if let Some(index) = DIRS.iter().position(|dir| *dir == path) {
        return Some(entry_meta(index, vfs::S_IFDIR | 0o555, FileKind::Dir, 0));
    }
    let (index, backed) = entry(path)?;
    let size = match backing_size(backed) {
        Some(size) => size,
        None if path == fhs::etc::LINUX_HOSTS => DEFAULT_HOSTS.len() as u64,
        None => return None,
    };
    let ino = DIRS.len() + index;
    Some(entry_meta(ino, vfs::S_IFREG | 0o444, FileKind::File, size))
}

fn entry_meta(index: usize, mode: u16, kind: FileKind, size: u64) -> Meta {
    Meta {
        ino: BASE_INO + index as u64,
        mode,
        uid: 0,
        gid: 0,
        size,
        kind,
        times: vfs::Times::default(),
    }
}

/// What the directory `dir` (`/etc` or one of [`DIRS`]) lists from this
/// module: `(name, is_directory)`, only the entries that exist right now.
/// `None` for any other directory.
pub(super) fn children(dir: &str) -> Option<Vec<(String, bool)>> {
    if dir != fhs::etc::LINUX_ETC && !is_dir(dir) {
        return None;
    }
    let below = |path: &str| {
        let (parent, name) = path.rsplit_once('/')?;
        (parent == dir).then(|| String::from(name))
    };
    let mut names: Vec<(String, bool)> = DIRS
        .iter()
        .filter_map(|sub| below(sub).map(|name| (name, true)))
        .collect();
    names.extend(
        BACKED
            .iter()
            .filter(|backed| meta(backed.linux).is_some())
            .filter_map(|backed| below(backed.linux).map(|name| (name, false))),
    );
    Some(names)
}
