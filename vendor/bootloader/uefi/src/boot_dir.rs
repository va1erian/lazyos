//! The directory this loader was started from (docs/grub-install-plan.md K4).
//!
//! Upstream `bootloader` 0.11.17 reads `kernel-x86_64`, `boot.json` and
//! `ramdisk` from the root of the volume it started from. Installed beside
//! Linux, GRUB chainloads `\EFI\lazyos\BOOTX64.EFI` from the distro's ESP,
//! and those names at the ESP root would sit beside every other EFI tool's
//! files. LazyOS first looks in the image's own directory (taken from the
//! loaded image's file path) and falls back to the volume root, so the stick
//! (`\EFI\BOOT\BOOTX64.EFI`, files at the root) is unchanged.
//!
//! Pure and `core`-only so it can be tested on the host:
//! `rustc --test --edition 2021 vendor/bootloader/uefi/src/boot_dir.rs`.

/// Longest path, in UCS-2 code units with the terminating nul, this module
/// builds; a longer one is not tried and the root is used.
pub const MAX_PATH: usize = 256;

const BACKSLASH: u16 = b'\\' as u16;
const SLASH: u16 = b'/' as u16;

/// `<directory of image_path>\<name>` plus a nul into `out`, returning the
/// length including the nul. `image_path` is the concatenation of the image
/// device path's file-path nodes (nuls are skipped; either separator is
/// accepted and written as `\`). `None` when the image sits at the volume
/// root, has no directory, or the result does not fit: the caller then reads
/// the root as upstream does.
pub fn join(
    image_path: impl Iterator<Item = u16>,
    name: impl Iterator<Item = u16>,
    out: &mut [u16; MAX_PATH],
) -> Option<usize> {
    let mut len = 0;
    let mut dir_end = None;
    for unit in image_path.filter(|&unit| unit != 0) {
        let unit = if unit == SLASH { BACKSLASH } else { unit };
        // Collapse the separators two file-path nodes put back to back.
        if unit == BACKSLASH && len > 0 && out[len - 1] == BACKSLASH {
            continue;
        }
        *out.get_mut(len)? = unit;
        len += 1;
        if unit == BACKSLASH {
            dir_end = Some(len);
        }
    }
    let mut at = dir_end?;
    // A leading separator alone is the root, which the caller tries anyway.
    if out[..at].iter().all(|&unit| unit == BACKSLASH) {
        return None;
    }
    for unit in name {
        *out.get_mut(at)? = unit;
        at += 1;
    }
    *out.get_mut(at)? = 0;
    Some(at + 1)
}

/// [`join`] over the image path's file-path nodes, a separator between
/// each. GRUB's `chainloader` gives the directory and the file as two nodes
/// (`\EFI\lazyos`, then `BOOTX64.EFI`, with no separator at the seam);
/// firmware and the shell give one. Doubled separators collapse in `join`.
pub fn join_nodes<N, U>(nodes: N, name: impl Iterator<Item = u16>, out: &mut [u16; MAX_PATH]) -> Option<usize>
where
    N: Iterator<Item = U>,
    U: Iterator<Item = u16>,
{
    let units = nodes.flat_map(|node| core::iter::once(BACKSLASH).chain(node));
    join(units, name, out)
}

#[cfg(test)]
mod tests {
    use super::{join, join_nodes, MAX_PATH};

    fn run(image: &str, name: &str) -> Option<String> {
        let mut out = [0u16; MAX_PATH];
        let len = join(image.encode_utf16(), name.encode_utf16(), &mut out)?;
        assert_eq!(out[len - 1], 0, "missing nul");
        Some(String::from_utf16(&out[..len - 1]).unwrap())
    }

    #[test]
    fn grub_chainload_path() {
        assert_eq!(
            run("\\EFI\\lazyos\\BOOTX64.EFI", "kernel-x86_64").as_deref(),
            Some("\\EFI\\lazyos\\kernel-x86_64")
        );
    }

    #[test]
    fn forward_slashes_and_split_nodes() {
        assert_eq!(
            run("/EFI/lazyos/\0\\BOOTX64.EFI\0", "ramdisk").as_deref(),
            Some("\\EFI\\lazyos\\ramdisk")
        );
    }

    fn run_nodes(nodes: &[&str], name: &str) -> Option<String> {
        let mut out = [0u16; MAX_PATH];
        let len = join_nodes(nodes.iter().map(|n| n.encode_utf16()), name.encode_utf16(), &mut out)?;
        Some(String::from_utf16(&out[..len - 1]).unwrap())
    }

    #[test]
    fn grub_two_node_path() {
        // grub_efi_file_device_path: the directory, then the file.
        assert_eq!(
            run_nodes(&["\\EFI\\lazyos", "BOOTX64.EFI"], "kernel-x86_64").as_deref(),
            Some("\\EFI\\lazyos\\kernel-x86_64")
        );
        assert_eq!(
            run_nodes(&["\\EFI\\lazyos\\BOOTX64.EFI"], "ramdisk").as_deref(),
            Some("\\EFI\\lazyos\\ramdisk")
        );
        assert_eq!(run_nodes(&["BOOTX64.EFI"], "ramdisk"), None);
    }

    #[test]
    fn root_or_no_directory_falls_back() {
        assert_eq!(run("\\BOOTX64.EFI", "ramdisk"), None);
        assert_eq!(run("BOOTX64.EFI", "ramdisk"), None);
        assert_eq!(run("", "ramdisk"), None);
    }

    #[test]
    fn removable_media_path() {
        assert_eq!(
            run("\\EFI\\BOOT\\BOOTX64.EFI", "kernel-x86_64").as_deref(),
            Some("\\EFI\\BOOT\\kernel-x86_64")
        );
    }

    #[test]
    fn too_long_is_refused() {
        let long = "\\a".repeat(MAX_PATH);
        assert_eq!(run(&long, "ramdisk"), None);
        let dir = format!("\\{}\\x.efi", "d".repeat(MAX_PATH - 8));
        assert_eq!(run(&dir, "kernel-x86_64"), None);
    }
}
