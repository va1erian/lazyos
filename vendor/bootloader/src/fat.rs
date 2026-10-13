use crate::file_data_source::FileDataSource;
use anyhow::Context;
use fatfs::Dir;
use std::fs::File;
use std::{collections::BTreeMap, fs, path::Path};

use crate::KERNEL_FILE_NAME;

/// The FAT partition size for `needed_size` bytes of files: the files, a margin that grows with
/// them, and 2 MiB of rounding and fixed overhead.
///
/// The margin is the allocation tables. `fatfs` formats a volume of 512 MiB or more as FAT32 with
/// 4 KiB clusters, so the two tables take 2 * 4 / 4096 of the volume (a little under 0.2 %, 2 MiB
/// per GiB); a fixed 2 MiB of slack, which is all this once added, ran out at about 1 GiB of files
/// (a ramdisk with the desktop's apps and a browser) as `No space left on device` while copying
/// the ramdisk. One part in 128 is four times the tables' share and costs little.
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

    const MB: u64 = 1024 * 1024;

    #[test]
    fn the_margin_covers_the_allocation_tables_at_every_size() {
        // FAT32, 4 KiB clusters: two tables of 4 bytes per cluster, plus 1 MiB for the reserved
        // sectors, the root directory and the per-file cluster rounding.
        for mb in [1u64, 64, 300, 512, 900, 1024, 1500, 2048, 3500] {
            let needed = mb * MB;
            let tables = 2 * 4 * (needed / 4096);
            assert!(
                fat_partition_size(needed) >= needed + tables + MB,
                "{mb} MiB of files"
            );
            assert_eq!(fat_partition_size(needed) % MB, 0);
        }
    }

    /// The failure this sizing fixes, for real: a 1.5 GiB file, which the old fixed slack could
    /// not hold. Writes 1.5 GiB, so it only runs when asked (`cargo test -- --ignored`).
    #[test]
    #[ignore]
    fn a_ramdisk_of_over_a_gibibyte_fits() {
        let dir = std::env::temp_dir().join(format!("fat-size-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ramdisk = dir.join("ramdisk");
        File::create(&ramdisk).unwrap().set_len(1536 * MB).unwrap();
        let source = FileDataSource::File(ramdisk);
        let mut files = BTreeMap::new();
        files.insert("ramdisk", &source);
        let out = dir.join("boot.fat");
        let result = create_fat_filesystem(files, &out);
        let _ = std::fs::remove_dir_all(&dir);
        result.unwrap();
    }
}
