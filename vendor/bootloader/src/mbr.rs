use anyhow::Context;
use mbrman::BOOT_ACTIVE;
use std::{
    fs::{self, File},
    io::{self, Seek, SeekFrom},
    path::Path,
};

const SECTOR_SIZE: u32 = 512;

/// Write an MBR disk image: the boot sector, the second stage, and a FAT
/// partition of `boot_partition_size` bytes that `fill` writes into the image
/// file (given the file and the partition's first byte). The image is
/// `partition start + boot_partition_size` bytes long. The open file is
/// returned: a caller that goes on with the image keeps the handle, since
/// opening a large file just written can wait a long time for the virus
/// scanner of the host.
pub fn create_mbr_disk(
    bootsector_binary: &[u8],
    second_stage_binary: &[u8],
    boot_partition_size: u64,
    out_mbr_path: &Path,
    fill: impl FnOnce(&File, u64) -> anyhow::Result<()>,
) -> anyhow::Result<File> {
    use std::io::Cursor;
    let mut boot_sector = Cursor::new(bootsector_binary);
    let mut mbr =
        mbrman::MBR::read_from(&mut boot_sector, SECTOR_SIZE).context("failed to read MBR")?;

    for (index, partition) in mbr.iter() {
        if !partition.is_unused() {
            anyhow::bail!("partition {index} should be unused");
        }
    }

    let mut second_stage = Cursor::new(second_stage_binary);
    let second_stage_size = second_stage_binary.len() as u64;

    let second_stage_start_sector = 1;
    let second_stage_sectors = ((second_stage_size - 1) / u64::from(SECTOR_SIZE) + 1)
        .try_into()
        .context("size of second stage is larger than u32::MAX")?;
    mbr[1] = mbrman::MBRPartitionEntry {
        boot: BOOT_ACTIVE,
        starting_lba: second_stage_start_sector,
        sectors: second_stage_sectors,
        // see BOOTLOADER_SECOND_STAGE_PARTITION_TYPE in `boot_sector` crate
        sys: 0x20,

        first_chs: mbrman::CHS::empty(),
        last_chs: mbrman::CHS::empty(),
    };

    let boot_partition_start_sector = second_stage_start_sector + second_stage_sectors;
    mbr[2] = mbrman::MBRPartitionEntry {
        boot: BOOT_ACTIVE,
        starting_lba: boot_partition_start_sector,
        sectors: ((boot_partition_size - 1) / u64::from(SECTOR_SIZE) + 1)
            .try_into()
            .context("size of FAT partition is larger than u32::MAX")?,
        //TODO: is this the correct type?
        sys: 0x0c, // FAT32 with LBA

        first_chs: mbrman::CHS::empty(),
        last_chs: mbrman::CHS::empty(),
    };

    let mut disk = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(out_mbr_path)
        .with_context(|| {
            format!(
                "failed to create MBR disk image at `{}`",
                out_mbr_path.display()
            )
        })?;

    mbr.write_into(&mut disk)
        .context("failed to write MBR header to disk image")?;

    // second stage
    assert_eq!(
        disk.stream_position()
            .context("failed to get disk image seek position")?,
        u64::from(second_stage_start_sector * SECTOR_SIZE)
    );
    io::copy(&mut second_stage, &mut disk)
        .context("failed to copy second stage binary to MBR disk image")?;

    // fat partition, written where it will live
    let boot_partition_start = u64::from(boot_partition_start_sector * SECTOR_SIZE);
    crate::sparse::mark_sparse(&disk);
    disk.set_len(boot_partition_start + boot_partition_size)
        .context("failed to set the length of the MBR disk image")?;
    disk.seek(SeekFrom::Start(boot_partition_start))
        .context("seek failed")?;
    fill(&disk, boot_partition_start).context("failed to write the FAT partition")?;
    Ok(disk)
}
