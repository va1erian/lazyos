//! The data path: requests reach the fake provider and come back intact,
//! large transfers split at the bounce buffer, a flush is a request, an ext2
//! volume lives on the disk, and the late `/home` mount finds it by label.

use super::*;
use crate::fs::bootcfg::VolumeId;
use crate::fs::vfs::{FsError, Id, MountFlags, Vfs};
use alloc::boxed::Box;
use alloc::sync::Arc;

const STAMP: i64 = 1_700_000_000;

fn lib_error(error: ext2fs::Ext2Error) -> String {
    format!("ext2fs: {error:?}")
}

fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

/// Write `sectors` patterned sectors at `lba`, read them back, compare with
/// what the fake stored.
fn roundtrip(disk: &dyn BlockDevice, lba: u64, sectors: usize, seed: u8) -> Result<(), String> {
    let mut out = vec![0u8; sectors * SECTOR_SIZE];
    for (index, sector) in out.chunks_mut(SECTOR_SIZE).enumerate() {
        pattern(lba + index as u64, seed, sector);
    }
    disk.write_sectors(lba, &out)
        .map_err(|e| format!("write {lba}+{sectors}: {e:?}"))?;
    let stored = with_fake(|fake| {
        let start = lba as usize * SECTOR_SIZE;
        fake.data.matches(start, &out)
    });
    check!(stored, "the provider stored other bytes at {lba}+{sectors}");
    let mut back = vec![0u8; out.len()];
    disk.read_sectors(lba, &mut back)
        .map_err(|e| format!("read {lba}+{sectors}: {e:?}"))?;
    check!(back == out, "read back other bytes at {lba}+{sectors}");
    Ok(())
}

pub fn read_write_roundtrip() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        check!(
            disk.sector_count() == SECTORS,
            "sector count {}",
            disk.sector_count()
        );
        check!(disk.is_writable(), "a writable disk reports read-only");
        roundtrip(disk, 0, 1, 1)?;
        roundtrip(disk, 7, 3, 2)?;
        roundtrip(disk, SECTORS - 1, 1, 3)?;
        // Out of range and partial sectors never reach the provider.
        let before = with_fake(|fake| fake.served);
        let mut sector = [0u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(SECTORS, &mut sector),
            BlockError::Bounds,
            "past the end",
        )?;
        expect_err(
            disk.write_sectors(0, &sector[..100]),
            BlockError::Unsupported,
            "partial",
        )?;
        check!(
            disk.read_sectors(0, &mut []).is_ok(),
            "an empty read failed"
        );
        check!(
            with_fake(|fake| fake.served) == before,
            "a refused request reached the provider"
        );
        let (stats, alive) = provider::stats(with_fake(|fake| fake.disk)).ok_or("no stats")?;
        check!(
            alive && stats.errors == 0 && stats.requests == 6,
            "stats {stats:?} alive {alive}"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn large_requests_split() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        // 200 KiB: four requests, the last one partial.
        roundtrip(disk, 100, 400, 4)?;
        let last = with_fake(|fake| fake.last).ok_or("no request seen")?;
        check!(
            last.op == Op::Read && last.lba == 100 + 384 && last.bytes == 16 * SECTOR_SIZE,
            "last chunk {last:?}"
        );
        check!(
            with_fake(|fake| fake.served) == 8,
            "{} requests for 2 x 4 chunks",
            with_fake(|f| f.served)
        );
        // Exactly one bounce buffer's worth is one request.
        let before = with_fake(|fake| fake.served);
        roundtrip(disk, 1000, provider::MAX_REQUEST_BYTES / SECTOR_SIZE, 5)?;
        check!(
            with_fake(|fake| fake.served) == before + 2,
            "64 KiB took more than one request each way"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn flush_reaches_the_driver() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        disk.flush().map_err(|e| format!("flush: {e:?}"))?;
        disk.flush().map_err(|e| format!("flush: {e:?}"))?;
        check!(
            with_fake(|fake| fake.flushes) == 2,
            "flushes {}",
            with_fake(|f| f.flushes)
        );
        let last = with_fake(|fake| fake.last).ok_or("no request")?;
        check!(
            last.op == Op::Flush && last.bytes == 0,
            "flush request {last:?}"
        );
        Ok(())
    })();
    teardown();
    result
}

/// An ext2 volume on the stick: format, mount through the kernel adapter,
/// write through the VFS, sync, and the library reads the file back clean.
pub fn ext2_on_a_stick() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let geometry = ext2fs::Geometry {
            block_size: 1024,
            blocks_count: 1024,
            bytes_per_inode: 8192,
        };
        ext2fs::format(&disk, geometry, "stick", [0x5A; 16], STAMP).map_err(lib_error)?;
        let volume = crate::fs::ext2::Ext2::open(disk).map_err(fs_error)?;
        let mut vfs = Vfs::new();
        vfs.mount("/", Arc::new(volume), MountFlags::default())
            .map_err(fs_error)?;
        let id = Id::ROOT;
        vfs.create(id, "/note", 0o644).map_err(fs_error)?;
        let body = [0xA5u8; 3000];
        vfs.write(id, "/note", 0, &body).map_err(fs_error)?;
        let flushes = with_fake(|fake| fake.flushes);
        vfs.sync_all().map_err(fs_error)?;
        check!(
            with_fake(|fake| fake.flushes) > flushes,
            "sync did not flush the stick"
        );
        let mut back = [0u8; 3000];
        let read = vfs.read(id, "/note", 0, &mut back).map_err(fs_error)?;
        check!(
            read == 3000 && back == body,
            "read back {read} bytes, wrong"
        );
        drop(vfs);
        let device: &'static dyn BlockDevice = disk;
        let library =
            ext2fs::Ext2::open(Box::new(device), crate::fs::vfs::now).map_err(lib_error)?;
        check!(library.was_clean_at_mount(), "the volume was left dirty");
        check!(
            library.read_file("/note").map_err(lib_error)? == body,
            "the library reads other bytes"
        );
        Ok(())
    })();
    teardown();
    result
}

/// A window of a disk, as the formatter's device (the partition the late
/// mount will find is only registered once `settle` scans the disk).
struct Window {
    disk: &'static dyn BlockDevice,
    start: u64,
    sectors: u64,
}

impl ext2fs::BlockIo for Window {
    fn sector_count(&self) -> u64 {
        self.sectors
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), ext2fs::IoError> {
        self.disk
            .read_sectors(self.start + lba, buf)
            .map_err(|_| ext2fs::IoError::Failed)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), ext2fs::IoError> {
        self.disk
            .write_sectors(self.start + lba, buf)
            .map_err(|_| ext2fs::IoError::Failed)
    }

    fn flush(&self) -> Result<(), ext2fs::IoError> {
        self.disk.flush().map_err(|_| ext2fs::IoError::Failed)
    }

    fn is_writable(&self) -> bool {
        true
    }
}

const PART_LBA: u64 = 64;

/// A mount table with a ramfs at `/`, as a booted system always has.
fn rooted() -> Result<Vfs, String> {
    let mut vfs = Vfs::new();
    vfs.mount(
        "/",
        Arc::new(crate::fs::ramfs::RamFs::new()),
        MountFlags::default(),
    )
    .map_err(fs_error)?;
    Ok(vfs)
}

/// An MBR with one Linux partition at [`PART_LBA`] to the end of the disk.
fn write_mbr(disk: &dyn BlockDevice) -> Result<(), String> {
    let mut mbr = [0u8; SECTOR_SIZE];
    let entry = &mut mbr[446..462];
    entry[4] = 0x83;
    entry[8..12].copy_from_slice(&(PART_LBA as u32).to_le_bytes());
    entry[12..16].copy_from_slice(&((SECTORS - PART_LBA) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    disk.write_sectors(0, &mbr)
        .map_err(|e| format!("mbr: {e:?}"))
}

fn label(text: &[u8]) -> [u8; 16] {
    let mut label = [0u8; 16];
    label[..text.len()].copy_from_slice(text);
    label
}

/// `settle` scans the stick's MBR, finds the ext2 partition labelled as
/// configured, and mounts it at `/home` in both tables with `nosuid`; a
/// second call reports it mounted, and a wrong label keeps waiting.
pub fn late_home_mount() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let native = crate::fs::install_native_for_test(rooted()?);
    let abi = crate::fs::install_abi_for_test(rooted()?);
    let result = (|| {
        write_mbr(disk)?;
        let window = Window {
            disk,
            start: PART_LBA,
            sectors: SECTORS - PART_LBA,
        };
        let geometry = ext2fs::Geometry {
            block_size: 1024,
            blocks_count: ((SECTORS - PART_LBA) / 2) as u32,
            bytes_per_inode: 8192,
        };
        ext2fs::format(&window, geometry, "lazyhome", [0x77; 16], STAMP).map_err(lib_error)?;
        let volume =
            ext2fs::Ext2::open(Box::new(window), crate::fs::vfs::now).map_err(lib_error)?;
        volume
            .mkdir_p("/user", 0o700, 1000, 1000)
            .map_err(lib_error)?;
        volume
            .write_file("/user/hello", b"from the stick", 0o644, 1000, 1000, STAMP)
            .map_err(lib_error)?;
        volume.flush().map_err(lib_error)?;
        drop(volume);

        // A label nobody carries. Before the provider's first scan is done
        // nothing is read (the disk is not even scanned) and the answer is
        // to wait; after it, the volume is absent.
        crate::fs::late::reset_for_tests(Some((
            VolumeId::Label(label(b"elsewhere")),
            MountFlags::default(),
        )));
        let partition = format!("{}p1", disk.name());
        check!(
            crate::fs::late::settle() == crate::fs::late::state::WAITING,
            "settled before the provider's first scan"
        );
        check!(
            crate::block::device(&partition).is_none(),
            "{partition} was scanned before the provider's first scan"
        );
        crate::fs::late::provider_scanned();
        check!(
            crate::fs::late::settle() == crate::fs::late::state::ABSENT,
            "a missing volume is not absent"
        );
        check!(
            crate::block::device(&partition).is_some(),
            "{partition} was not registered"
        );

        let flags = MountFlags {
            noexec: true,
            ..Default::default()
        };
        crate::fs::late::reset_for_tests(Some((VolumeId::Label(label(b"lazyhome")), flags)));
        crate::fs::late::provider_scanned();
        let state = crate::fs::late::settle();
        check!(
            state == crate::fs::late::state::MOUNTED,
            "settle returned {state}"
        );
        check!(
            crate::fs::late::settle() == crate::fs::late::state::MOUNTED_EARLIER,
            "a second settle remounted"
        );
        let (mut mine, _) =
            crate::fs::install_native_for_test(Vfs::new()).ok_or("no native table")?;
        let mut mine_abi = crate::fs::install_abi_for_test(Vfs::new()).ok_or("no ABI table")?;
        for (table, what) in [(&mut mine, "native"), (&mut mine_abi, "abi")] {
            let mounted = table.mounts();
            check!(
                mounted
                    .iter()
                    .any(|(point, _)| point.as_str() == fhs::mount::HOME),
                "{what}: /home not mounted: {mounted:?}"
            );
            let flags = table.mount_flags("/home/user/hello");
            check!(
                flags.nosuid && flags.noexec && !flags.ro,
                "{what}: flags {flags:?}"
            );
            let mut buf = [0u8; 32];
            let read = table
                .read(Id::ROOT, "/home/user/hello", 0, &mut buf)
                .map_err(fs_error)?;
            check!(&buf[..read] == b"from the stick", "{what}: read back wrong");
        }
        mine.create(Id::ROOT, "/home/user/new", 0o600)
            .map_err(fs_error)?;
        mine.write(Id::ROOT, "/home/user/new", 0, b"kernel")
            .map_err(fs_error)?;
        mine.sync_all().map_err(fs_error)?;
        let window = Window {
            disk,
            start: PART_LBA,
            sectors: SECTORS - PART_LBA,
        };
        drop(mine);
        drop(mine_abi);
        let volume =
            ext2fs::Ext2::open(Box::new(window), crate::fs::vfs::now).map_err(lib_error)?;
        check!(
            volume.was_clean_at_mount(),
            "the home volume was left dirty"
        );
        check!(
            volume.read_file("/user/new").map_err(lib_error)? == b"kernel",
            "the write did not land"
        );
        Ok(())
    })();
    crate::fs::late::reset_for_tests(None);
    crate::fs::restore_native_for_test(native);
    crate::fs::restore_abi_for_test(abi);
    teardown();
    result
}

/// The part of the disk the stress test works on (512 KiB: twice the
/// largest request, and a small heap footprint for it and its shadow).
const STRESS_SECTORS: u64 = 1024;

/// Thousands of mixed requests against a provider that fails one in seven:
/// every failure is clean, every success matches a shadow copy, the disk stays
/// alive, and the heap does not grow.
pub fn stress() -> Result<(), String> {
    let disk = setup(Mode::FlakyEvery(7))?;
    let result = (|| {
        let mut shadow = super::Store::new(STRESS_SECTORS as usize * SECTOR_SIZE);
        let mut buf = vec![0u8; 160 * SECTOR_SIZE];
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
        let mut before = 0;
        let (mut ok, mut failed) = (0u32, 0u32);
        for round in 0..3000u32 {
            if round == 100 {
                before = measure();
            }
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let sectors = 1 + (seed % 160) as usize;
            let lba = (seed >> 16) % (STRESS_SECTORS - sectors as u64);
            let range = lba as usize * SECTOR_SIZE..(lba as usize + sectors) * SECTOR_SIZE;
            let chunk = &mut buf[..sectors * SECTOR_SIZE];
            let outcome = match seed >> 60 {
                0..=6 => {
                    for (index, sector) in chunk.chunks_mut(SECTOR_SIZE).enumerate() {
                        pattern(lba + index as u64, round as u8, sector);
                    }
                    let result = disk.write_sectors(lba, chunk);
                    if result.is_ok() {
                        shadow.write(range.start, chunk);
                    } else {
                        // A failed write may have landed partly (earlier
                        // chunks): take the provider's word for it.
                        with_fake(|fake| fake.data.read(range.start, chunk));
                        shadow.write(range.start, chunk);
                    }
                    result
                }
                7 => disk.flush(),
                _ => {
                    let result = disk.read_sectors(lba, chunk);
                    if result.is_ok() && !shadow.matches(range.start, chunk) {
                        return Err(format!("round {round}: read {lba}+{sectors} differs"));
                    }
                    result
                }
            };
            match outcome {
                Ok(()) => ok += 1,
                Err(BlockError::Io) => failed += 1,
                Err(other) => return Err(format!("round {round}: {other:?}")),
            }
        }
        let after = measure();
        let (stats, alive) = provider::stats(with_fake(|fake| fake.disk)).ok_or("no stats")?;
        check!(alive, "the disk died under transient errors");
        check!(ok > 2000 && failed > 100, "ok {ok} failed {failed}");
        check!(stats.timeouts == 0 && stats.stale == 0, "stats {stats:?}");
        check!(
            after <= before + 16 * 1024,
            "heap grew from {before} to {after}"
        );
        Ok(())
    })();
    teardown();
    result
}
