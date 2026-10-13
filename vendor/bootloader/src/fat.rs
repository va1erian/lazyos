use crate::file_data_source::FileDataSource;
use anyhow::Context;
use fatfs::Dir;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
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

/// The size of the FAT partition holding `files`.
pub fn partition_size(files: &BTreeMap<&str, &FileDataSource>) -> anyhow::Result<u64> {
    let mut needed_size = 0;
    for source in files.values() {
        needed_size += source.len()?;
    }
    Ok(fat_partition_size(needed_size))
}

pub fn create_fat_filesystem(
    files: BTreeMap<&str, &FileDataSource>,
    out_fat_path: &Path,
) -> anyhow::Result<()> {
    let size = partition_size(&files)?;
    // create new filesystem image file at the given path and set its length
    let fat_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(out_fat_path)
        .unwrap();
    crate::sparse::mark_sparse(&fat_file);
    fat_file.set_len(size).unwrap();
    create_fat_filesystem_at(files, &fat_file, 0, size)
}

/// Write the FAT filesystem holding `files` into the `size` bytes of `file`
/// that start at byte `offset` (the file is at least `offset + size` long).
///
/// A disk image built this way holds its FAT partition from the first write:
/// the partition is not made in a file of its own and copied in afterwards,
/// which for a stick with a ramdisk of 600 MiB or more was three passes over
/// the data (the FAT file, the disk image, and a read back to place the next
/// partition).
///
/// The `size` bytes must be new (a file that was just created and extended):
/// a write of zeros is not made, so that a ramdisk's free space, which is most
/// of one, stays a hole of a sparse file instead of being written.
pub fn create_fat_filesystem_at(
    files: BTreeMap<&str, &FileDataSource>,
    file: &File,
    offset: u64,
    size: u64,
) -> anyhow::Result<()> {
    let window = || Window {
        file,
        start: offset,
        len: size,
        position: 0,
    };

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
    fatfs::format_volume(window(), format_options).context("Failed to format FAT file")?;
    let filesystem = fatfs::FileSystem::new(window(), fatfs::FsOptions::new())
        .context("Failed to open FAT file system of UEFI FAT file")?;
    let root_dir = filesystem.root_dir();

    // copy files to file system
    add_files_to_image(&root_dir, files)
}

/// A byte range of a file as a stream of its own, so `fatfs` can format a
/// partition in the middle of a disk image.
struct Window<'a> {
    file: &'a File,
    start: u64,
    len: u64,
    position: u64,
}

impl Window<'_> {
    /// How many of `wanted` bytes fit before the end of the range.
    fn room(&self, wanted: usize) -> usize {
        wanted.min(self.len.saturating_sub(self.position) as usize)
    }
}

impl Read for Window<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let len = self.room(buf.len());
        if len == 0 {
            return Ok(0);
        }
        let mut file = self.file;
        file.seek(SeekFrom::Start(self.start + self.position))?;
        let read = file.read(&mut buf[..len])?;
        self.position += read as u64;
        Ok(read)
    }
}

impl Write for Window<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let len = self.room(buf.len());
        if len == 0 && !buf.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "write past the end of the FAT partition",
            ));
        }
        if buf[..len].iter().all(|&byte| byte == 0) {
            self.position += len as u64;
            return Ok(len);
        }
        let mut file = self.file;
        file.seek(SeekFrom::Start(self.start + self.position))?;
        let written = file.write(&buf[..len])?;
        self.position += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut file = self.file;
        file.flush()
    }
}

impl Seek for Window<'_> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(at) => Some(at),
            SeekFrom::End(delta) => self.len.checked_add_signed(delta),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
        };
        match target {
            Some(at) => {
                self.position = at;
                Ok(at)
            }
            None => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the FAT partition",
            )),
        }
    }
}

pub fn add_files_to_image<IO: fatfs::ReadWriteSeek>(
    root_dir: &Dir<'_, IO>,
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

    fn fresh(dir: &std::path::Path, name: &str, len: u64) -> File {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(dir.join(name))
            .unwrap();
        file.set_len(len).unwrap();
        file
    }

    /// A FAT partition in the middle of a file holds what a partition of its own
    /// holds, with the zeros of a ramdisk's free space never written: the same
    /// bytes read back, whatever is around it.
    #[test]
    fn a_partition_in_a_disk_image_matches_one_in_a_file_of_its_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = vec![0u8; 3 * MB as usize];
        data[..1000].fill(0xAB);
        data[MB as usize + 17..MB as usize + 4096].fill(0xCD);
        // Everything else (two thirds of it) is zero.
        let source = FileDataSource::Data(data.clone());
        let kernel = FileDataSource::Data(b"kernel bytes".to_vec());
        let mut files = BTreeMap::new();
        files.insert("kernel-x86_64", &kernel);
        files.insert("ramdisk", &source);
        let size = partition_size(&files).unwrap();

        let alone = dir.path().join("alone.fat");
        create_fat_filesystem(files.clone(), &alone).unwrap();

        let offset = 4096 + 512;
        let image = fresh(dir.path(), "disk.img", offset + size);
        create_fat_filesystem_at(files, &image, offset, size).unwrap();

        let disk = fs::read(dir.path().join("disk.img")).unwrap();
        assert_eq!(&disk[offset as usize..], &fs::read(&alone).unwrap()[..]);
        assert!(disk[..offset as usize].iter().all(|&byte| byte == 0), "before it: untouched");
        let fat = fs::File::open(dir.path().join("disk.img")).unwrap();
        let window = Window { file: &fat, start: offset, len: size, position: 0 };
        let filesystem = fatfs::FileSystem::new(window, fatfs::FsOptions::new()).unwrap();
        let mut read = Vec::new();
        std::io::Read::read_to_end(&mut filesystem.root_dir().open_file("ramdisk").unwrap(), &mut read)
            .unwrap();
        assert_eq!(read, data);
    }

    #[test]
    fn the_window_ends_where_the_partition_ends() {
        let dir = tempfile::tempdir().unwrap();
        let file = fresh(dir.path(), "w.img", 8192);
        let mut window = Window { file: &file, start: 1024, len: 4096, position: 0 };
        assert_eq!(window.seek(std::io::SeekFrom::End(0)).unwrap(), 4096);
        assert_eq!(std::io::Read::read(&mut window, &mut [0u8; 16]).unwrap(), 0, "at the end");
        assert!(window.seek(std::io::SeekFrom::End(-5000)).is_err(), "before the start");
        window.seek(std::io::SeekFrom::Start(4090)).unwrap();
        // A short write at the end, then nothing more fits.
        assert_eq!(std::io::Write::write(&mut window, &[7u8; 16]).unwrap(), 6);
        assert!(std::io::Write::write(&mut window, &[7u8; 1]).is_err());
        // Zeros past nothing are still an error, and zeros inside are not written.
        window.seek(std::io::SeekFrom::Start(0)).unwrap();
        assert_eq!(std::io::Write::write(&mut window, &[0u8; 100]).unwrap(), 100);
        assert_eq!(window.position, 100);
        let on_disk = fs::read(dir.path().join("w.img")).unwrap();
        assert_eq!(&on_disk[1024 + 4090..1024 + 4096], &[7u8; 6]);
        assert!(on_disk[1024..1024 + 100].iter().all(|&byte| byte == 0));
    }
}
