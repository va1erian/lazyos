//! Synthetic `/proc` files that describe the system rather than a process:
//! `/proc/mounts` (also reached as `/proc/self/mounts`) and
//! `/proc/self/mountinfo`. Tools such as BusyBox `df` and `mount` find out
//! what is mounted by reading them.
//!
//! There is no procfs mount: like the other fabricated entries of
//! [`super::path`] these are answered by name, and their bytes are rebuilt
//! from the ABI mount table each time they are opened (or stat'ed), so they
//! always match what [`crate::fs::abi_mounts`] holds. They are read-only.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, Meta};

/// Inode numbers of the fabricated files (distinct from the synthetic
/// directories' 1).
const MOUNTS_INO: u64 = 2;
const MOUNTINFO_INO: u64 = 3;

/// Which fabricated file a path names.
#[derive(Clone, Copy)]
enum Kind {
    Mounts,
    MountInfo,
}

impl Kind {
    fn at(path: &str) -> Option<Kind> {
        match path {
            "/proc/mounts" | "/proc/self/mounts" => Some(Kind::Mounts),
            "/proc/self/mountinfo" => Some(Kind::MountInfo),
            _ => None,
        }
    }

    fn ino(self) -> u64 {
        match self {
            Kind::Mounts => MOUNTS_INO,
            Kind::MountInfo => MOUNTINFO_INO,
        }
    }
}

/// One row of the mount table, as a `/proc` reader wants to see it.
struct Mount {
    point: String,
    fs_type: &'static str,
    read_only: bool,
    /// Extra option words from the mount flags (`,noexec,nosuid`).
    options: String,
}

impl Mount {
    /// Describe a mount from the VFS's `(mount point, filesystem name)` pair.
    ///
    /// The name is the diagnostic one filesystems already publish
    /// (`"ext2 (rw)"`, `"fat16 (ro)"`, `"ramfs"`, `"overlay (abi rw)"`): its
    /// first word is the type and a `(ro)` marks a mount that cannot be
    /// written. The type is reported under the name Linux tools know it by.
    fn from_vfs(point: String, name: &'static str, flags: vfs::MountFlags) -> Mount {
        let fs_type = match name.split(' ').next().unwrap_or(name) {
            "fat16" => "vfat",
            other => other,
        };
        Mount {
            point,
            fs_type,
            read_only: name.contains("(ro)") || flags.ro,
            options: flags.proc_suffix(),
        }
    }

    /// The `ro`/`rw` mount option.
    fn access(&self) -> &'static str {
        if self.read_only {
            "ro"
        } else {
            "rw"
        }
    }
}

fn mount_table() -> Vec<Mount> {
    crate::fs::abi_mounts()
        .into_iter()
        .map(|(point, name)| {
            let flags = crate::fs::abi_mount_flags(&point);
            Mount::from_vfs(point, name, flags)
        })
        .collect()
}

/// Escape a field the way the kernel does, so a space in a path cannot be
/// mistaken for a column break: space, tab, newline and backslash become
/// three-digit octal escapes.
fn escape(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for ch in field.chars() {
        match ch {
            ' ' | '\t' | '\n' | '\\' => out.push_str(&format!("\\{:03o}", ch as u32)),
            _ => out.push(ch),
        }
    }
    out
}

/// `/proc/mounts`: `source mount-point type options dump pass`. There is no
/// block-device node to name, so the source repeats the type, as it does for
/// Linux's own pseudo filesystems.
fn render_mounts(table: &[Mount]) -> String {
    let mut out = String::new();
    for mount in table {
        out.push_str(&format!(
            "{} {} {} {}{} 0 0\n",
            mount.fs_type,
            escape(&mount.point),
            mount.fs_type,
            mount.access(),
            mount.options
        ));
    }
    out
}

/// `/proc/self/mountinfo`: `id parent major:minor root point options - type
/// source super-options`. Every mount is a top-level one (parent: the root),
/// with an anonymous `0:N` device number.
fn render_mountinfo(table: &[Mount]) -> String {
    let mut out = String::new();
    for (index, mount) in table.iter().enumerate() {
        let id = index + 1;
        let parent = if index == 0 { id } else { 1 };
        out.push_str(&format!(
            "{id} {parent} 0:{id} / {} {access}{extra} - {ty} {ty} {access}\n",
            escape(&mount.point),
            access = mount.access(),
            extra = mount.options,
            ty = mount.fs_type,
        ));
    }
    out
}

/// The bytes of the fabricated file at `path`, if it is one.
pub(super) fn contents(path: &str) -> Option<Vec<u8>> {
    let kind = Kind::at(path)?;
    let table = mount_table();
    let text = match kind {
        Kind::Mounts => render_mounts(&table),
        Kind::MountInfo => render_mountinfo(&table),
    };
    Some(text.into_bytes())
}

/// Metadata for the fabricated file at `path`: a world-readable regular file
/// as long as its current contents.
pub(super) fn meta(path: &str) -> Option<Meta> {
    let kind = Kind::at(path)?;
    let size = contents(path)?.len() as u64;
    Some(Meta {
        ino: kind.ino(),
        mode: vfs::S_IFREG | 0o444,
        uid: 0,
        gid: 0,
        size,
        kind: FileKind::File,
        // Generated on open, so there is no time to report.
        times: vfs::Times::default(),
    })
}
