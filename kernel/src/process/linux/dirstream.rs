//! Directory snapshots for `getdents64`: a directory is read once into a
//! `linux_dirent64` byte stream when it is opened (the fd table stores byte
//! snapshots, not directory handles), and [`super::dents`] hands the records
//! out. The ABI VFS supplies real directories' entries; a synthetic directory
//! (`/etc`, `/proc`, `/etc/ssl`) lists what the kernel fabricates there.

use alloc::vec::Vec;

use crate::fs::vfs::{FileKind, FsError, Id};

fn push_dirent(out: &mut Vec<u8>, ino: u64, d_type: u8, name: &str) {
    let start = out.len();
    out.extend_from_slice(&ino.to_le_bytes()); // d_ino
    out.extend_from_slice(&0u64.to_le_bytes()); // d_off
    out.extend_from_slice(&0u16.to_le_bytes()); // d_reclen (patched below)
    out.push(d_type);
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    while !(out.len() - start).is_multiple_of(8) {
        out.push(0);
    }
    let reclen = (out.len() - start) as u16;
    out[start + 16..start + 18].copy_from_slice(&reclen.to_le_bytes());
}

/// The `linux_dirent64` type byte for a VFS node kind.
fn dtype_of(kind: FileKind) -> u8 {
    const DT_DIR: u8 = 4;
    const DT_REG: u8 = 8;
    match kind {
        FileKind::Dir => DT_DIR,
        FileKind::File => DT_REG,
    }
}

/// The `.`/`..` prefix every directory stream starts with.
fn empty_dir_stream() -> Vec<u8> {
    const DT_DIR: u8 = 4;
    let mut out = Vec::new();
    push_dirent(&mut out, 1, DT_DIR, ".");
    push_dirent(&mut out, 1, DT_DIR, "..");
    out
}

/// Build a `linux_dirent64` stream for a directory, so `getdents64` can read
/// it like a file. `.`/`..` are added here.
pub(super) fn dir_stream(path: &str) -> Result<Vec<u8>, FsError> {
    let mut out = empty_dir_stream();
    let entries = match crate::fs::abi_readdir(Id::current(), path) {
        Ok(entries) => entries,
        Err(FsError::NotFound) => {
            // A synthetic directory lists what it fabricates.
            let names = super::procfs::children(path).ok_or(FsError::NotFound)?;
            for (index, (name, dir)) in names.iter().enumerate() {
                let kind = if *dir { FileKind::Dir } else { FileKind::File };
                push_dirent(&mut out, 100 + index as u64, dtype_of(kind), name);
            }
            return Ok(out);
        }
        Err(error) => return Err(error),
    };
    for entry in entries {
        push_dirent(&mut out, entry.ino, dtype_of(entry.kind), &entry.name);
    }
    Ok(out)
}
