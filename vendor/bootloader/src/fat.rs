use crate::file_data_source::FileDataSource;
use anyhow::Context;
use fatfs::Dir;
use std::fs::File;
use std::{collections::BTreeMap, fs, path::Path};

use crate::KERNEL_FILE_NAME;

/// The FAT partition size for `needed_size` bytes of files: the files, a margin that grows with
/// them, and between 1 and 2 MiB of rounding and fixed overhead (the reserved sectors, the root
/// directory and the cluster each file is rounded up to).
///
/// The margin is the allocation tables. `fatfs` formats a volume of 512 MiB up to 8 GiB as FAT32
/// with 4 KiB clusters, so the two tables take 2 * 4 / 4096 of the volume (a little under 0.2 %,
/// 2 MiB per GiB); a smaller volume is FAT16 or FAT12, whose tables are at most 0.4 %. The fixed
/// slack this once added alone ran out between 0.6 and 1 GiB of files, depending on their size
/// modulo 1 MiB (a ramdisk with the desktop's apps and a browser), as `No space left on device`
/// while copying the ramdisk. One part in 128 (0.8 %) is twice the worst case of any size.
fn fat_partition_size(needed_size: u64) -> u64 {
    const MB: u64 = 1024 * 1024;
    let margin = needed_size / 128;
    ((needed_size + margin + 1024 * 64 - 1) / MB + 1) * MB + MB
}

pub fn create_fat_filesystem(
    files: BTreeMap<&str, &FileDataSource>,
    out_fat_path: &Path,
) -> anyhow::Result<()> {
    // calculate needed size
    let mut needed_size = 0;
    for source in files.values() {
        needed_size += source.len()?;
    }

    // create new filesystem image file at the given path and set its length
    let fat_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(out_fat_path)
        .unwrap();
    let fat_size_padded_and_rounded = fat_partition_size(needed_size);
    fat_file.set_len(fat_size_padded_and_rounded).unwrap();

    // choose a file system label
    let mut label = *b"MY_RUST_OS!";

    // This __should__ always be a file, but maybe not. Should we allow the caller to set the volume label instead?
    if let Some(FileDataSource::File(path)) = files.get(KERNEL_FILE_NAME)
        && let Some(name) = path.file_stem()
    {
        let converted = name.to_string_lossy();
        let name = converted.as_bytes();
        let mut new_label = [0u8; 11];
        let name = &name[..usize::min(new_label.len(), name.len())];
        let slice = &mut new_label[..name.len()];
        slice.copy_from_slice(name);
        label = new_label;
    }

    // format the file system and open it
    let format_options = fatfs::FormatVolumeOptions::new().volume_label(label);
    fatfs::format_volume(&fat_file, format_options).context("Failed to format FAT file")?;
    let filesystem = fatfs::FileSystem::new(&fat_file, fatfs::FsOptions::new())
        .context("Failed to open FAT file system of UEFI FAT file")?;
    let root_dir = filesystem.root_dir();

    // copy files to file system
    add_files_to_image(&root_dir, files)
}

pub fn add_files_to_image(
    root_dir: &Dir<&File>,
    files: BTreeMap<&str, &FileDataSource>,
) -> anyhow::Result<()> {
    for (target_path_raw, source) in files {
        let target_path = Path::new(target_path_raw);
        // create parent directories
        let ancestors: Vec<_> = target_path.ancestors().skip(1).collect();
        for ancestor in ancestors.into_iter().rev().skip(1) {
            root_dir
                .create_dir(&ancestor.display().to_string())
                .with_context(|| {
                    format!(
                        "failed to create directory `{}` on FAT filesystem",
                        ancestor.display()
                    )
                })?;
        }

        let mut new_file = root_dir
            .create_file(target_path_raw)
            .with_context(|| format!("failed to create file at `{}`", target_path.display()))?;
        new_file.truncate().unwrap();

        source.copy_to(&mut new_file).with_context(|| {
            format!(
                "failed to copy source data `{:?}` to file at `{}`",
                source,
                target_path.display()
            )
        })?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Seek;

    const MB: u64 = 1024 * 1024;

    /// `fatfs` formats a partition of `fat_partition_size(needed)` bytes: its capacity must hold
    /// `needed` and room for a few files and directories (a kernel, a loader, `efi/boot`).
    #[test]
    fn the_formatted_volume_holds_the_files_at_every_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("volume.fat");
        // Both sides of each fatfs geometry change (FAT12/16/32, cluster size) and of the old
        // sizing's first failures (about 620 MiB, and 1 GiB at a round size).
        let sizes = [0, 1, MB, 3 * MB, 4 * MB, 16 * MB, 100 * MB, 300 * MB, 511 * MB, 512 * MB,
                     620 * MB + 12345, 900 * MB, 1024 * MB, 1536 * MB, 2047 * MB + 99];
        for needed in sizes {
            let size = fat_partition_size(needed);
            assert_eq!(size % MB, 0, "{needed} bytes");
            let file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)
                .unwrap();
            file.set_len(size).unwrap();
            fatfs::format_volume(&file, fatfs::FormatVolumeOptions::new()).unwrap();
            let fs = fatfs::FileSystem::new(&file, fatfs::FsOptions::new()).unwrap();
            let stats = fs.stats().unwrap();
            let capacity = u64::from(stats.total_clusters()) * u64::from(stats.cluster_size());
            // 64 KiB: eight files and three directories at up to 8 KiB each.
            assert!(capacity >= needed + 64 * 1024, "{needed} bytes: capacity {capacity}");
        }
    }

    /// The failure this sizing fixes, for real: a 1.5 GiB file, which the old fixed slack could
    /// not hold. Writes 1.5 GiB, so it only runs when asked (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn a_ramdisk_of_over_a_gibibyte_fits() {
        let dir = tempfile::tempdir().unwrap();
        let ramdisk = dir.path().join("ramdisk");
        File::create(&ramdisk).unwrap().set_len(1536 * MB).unwrap();
        let source = FileDataSource::File(ramdisk);
        let mut files = BTreeMap::new();
        files.insert("ramdisk", &source);
        let out = dir.path().join("boot.fat");
        create_fat_filesystem(files, &out).unwrap();
        // The copy is whole: the FAT image holds the file at its full length.
        let fat = File::open(&out).unwrap();
        let fs = fatfs::FileSystem::new(&fat, fatfs::FsOptions::new()).unwrap();
        let copied = fs.root_dir().open_file("ramdisk").unwrap().seek(std::io::SeekFrom::End(0)).unwrap();
        assert_eq!(copied, 1536 * MB);
    }
}
