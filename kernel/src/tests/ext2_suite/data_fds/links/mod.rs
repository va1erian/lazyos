//! Hard links (issue #871) through the Linux syscalls, on the ext2 `/data`
//! volume and the `/tmp` ramfs: `link`/`linkat` and their refusals, the
//! link count `stat` and `fstat` report, an open file reached by several
//! names (one registry entry, one page cache, one set of locks), unlink and
//! rename of one name of a linked file (no `.unlinked-*` parking while
//! another name is left), the ramfs's own bookkeeping, and soaks of
//! link/unlink/rename cycles and of shared mappings through two names.

use super::filemap::{self, sys6, Space, BASE, MAP_SHARED, PAGE, RW};
use super::*;

mod blob;
mod mapping;
mod names;
mod open;
mod ramfs;
mod soak;

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    ("linux_link_names_share_a_file", names::names_share_a_file),
    ("linux_link_refusals", names::refusals),
    ("linux_link_protected_hardlinks", names::protected_hardlinks),
    ("linux_link_read_only_volume", names::read_only_volume),
    ("linux_link_survives_remount", names::survives_remount),
    ("linux_link_unlink_while_open", open::unlink_while_open),
    ("linux_link_renames", open::renames),
    ("linux_link_one_registry_entry", open::one_registry_entry),
    ("linux_link_locks_shared", open::locks_shared_by_names),
    (
        "linux_link_stale_entry_does_not_hide_open_file",
        open::stale_entry_does_not_hide_the_open_file,
    ),
    (
        "linux_link_shared_mapping",
        mapping::shared_mapping_through_two_names,
    ),
    (
        "fs_ramfs_link_counts_and_charges",
        ramfs::counts_and_charges,
    ),
    ("fs_ramfs_dir_link_counts", ramfs::directory_link_counts),
    (
        "linux_filemap_cache_body_blob_links",
        blob::blob_storage_cycle,
    ),
    ("linux_link_soak_cycles", soak::link_unlink_rename_cycles),
    ("linux_link_soak_mapping", soak::mappings_through_names),
];

const SYS_STAT: u64 = 4;
const SYS_FLOCK: u64 = 73;
const SYS_LINK: u64 = 86;
const SYS_SYMLINK: u64 = 88;
const SYS_RMDIR: u64 = 84;
const SYS_CHMOD: u64 = 90;
const SYS_LINKAT: u64 = 265;

const EPERM: u64 = 1;
const EAGAIN: u64 = 11;
const EXDEV: u64 = 18;

const AT_SYMLINK_FOLLOW: u64 = 0x400;
const AT_EMPTY_PATH: u64 = 0x1000;
const O_DIRECTORY: u64 = 0o200000;

/// The two kinds of filesystem that hold links.
const BASES: [&str; 2] = ["/data", "/tmp"];

/// `link(old, new)`.
fn link(old: &str, new: &str) -> u64 {
    let (old, new) = (cstr(old), cstr(new));
    syscall(SYS_LINK, old.as_ptr() as u64, new.as_ptr() as u64, 0, 0)
}

/// `linkat(old_dirfd, old, new_dirfd, new, flags)`.
fn linkat(old_dirfd: u64, old: &str, new_dirfd: u64, new: &str, flags: u64) -> u64 {
    let (old, new) = (cstr(old), cstr(new));
    sys6(
        SYS_LINKAT,
        [
            old_dirfd,
            old.as_ptr() as u64,
            new_dirfd,
            new.as_ptr() as u64,
            flags,
            0,
        ],
    )
}

/// `(st_ino, st_nlink)` of `path` as `stat` reports them.
fn ino_links(path: &str) -> Result<(u64, u64), String> {
    let mut stat = [0u8; 144];
    let name = cstr(path);
    let ret = syscall(
        SYS_STAT,
        name.as_ptr() as u64,
        stat.as_mut_ptr() as u64,
        0,
        0,
    );
    check!(ret == 0, "stat({path}) returned {ret:#x}");
    Ok((word(&stat, 8), word(&stat, 16)))
}

/// `st_nlink` of descriptor `fd`.
fn fd_links(fd: u64) -> Result<u64, String> {
    let mut stat = [0u8; 144];
    let ret = syscall(SYS_FSTAT, fd, stat.as_mut_ptr() as u64, 0, 0);
    check!(ret == 0, "fstat({fd}) returned {ret:#x}");
    Ok(word(&stat, 16))
}

fn word(stat: &[u8; 144], offset: usize) -> u64 {
    u64::from_le_bytes(stat[offset..offset + 8].try_into().unwrap())
}

/// Reserved `.unlinked-*` names directly in `dir`.
fn parked_in(dir: &str) -> Result<usize, String> {
    let entries = crate::fs::abi_readdir(Id::ROOT, dir).map_err(fs_error)?;
    Ok(entries
        .iter()
        .filter(|entry| entry.name.starts_with(".unlinked-"))
        .count())
}

/// Live nodes and bytes of the `/tmp` ramfs, as `statfs` counts them.
fn tmp_usage() -> Result<(u64, u64), String> {
    let stat = crate::fs::abi_statfs(Id::ROOT, "/tmp").map_err(fs_error)?;
    Ok((stat.files_free, stat.blocks_free))
}

/// The open file behind descriptor `fd`.
fn file_of(fd: u64) -> Result<Arc<crate::fs::openfile::OpenFile>, String> {
    match task::fd_clone(fd as usize) {
        Some(task::Fd::Vfs { ref file }) => Ok(Arc::clone(file)),
        _ => Err(format!("descriptor {fd} is not a VFS file")),
    }
}
