//! The provider disk's read cache (`block/provider/readcache.rs`): repeated
//! reads stay off the stick, writes go through in order and update what was
//! cached, a failed write forgets, a dead disk fails whatever it once read,
//! streams bypass it, and a soak of random reads and writes always agrees
//! with what the device holds.

use super::*;
use crate::block::provider::readcache::{ReadCache, PAGE_SECTORS};

fn served() -> u64 {
    with_fake(|fake| fake.served)
}

fn disk_id() -> usize {
    with_fake(|fake| fake.disk)
}

/// What the fake device holds at `lba..lba + sectors`.
fn device_bytes(lba: u64, sectors: usize) -> Vec<u8> {
    let mut out = vec![0u8; sectors * SECTOR_SIZE];
    with_fake(|fake| fake.data.read(lba as usize * SECTOR_SIZE, &mut out));
    out
}

fn write_pattern(disk: &dyn BlockDevice, lba: u64, sectors: usize, seed: u8) -> Result<(), String> {
    let mut data = vec![0u8; sectors * SECTOR_SIZE];
    for (index, sector) in data.chunks_mut(SECTOR_SIZE).enumerate() {
        pattern(lba + index as u64, seed, sector);
    }
    disk.write_sectors(lba, &data)
        .map_err(|e| format!("write {lba}+{sectors}: {e:?}"))
}

fn read_back(disk: &dyn BlockDevice, lba: u64, sectors: usize) -> Result<Vec<u8>, String> {
    let mut out = vec![0u8; sectors * SECTOR_SIZE];
    disk.read_sectors(lba, &mut out)
        .map_err(|e| format!("read {lba}+{sectors}: {e:?}"))?;
    Ok(out)
}

pub fn repeated_reads_stay_off_the_stick() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        write_pattern(disk, 64, 8, 1)?;
        let before = served();
        let first = read_back(disk, 66, 1)?;
        check!(served() == before + 1, "the miss was not one request");
        // The same sector, a neighbour in its page, and the whole page.
        let again = read_back(disk, 66, 1)?;
        let near = read_back(disk, 69, 1)?;
        let page = read_back(disk, 64, 8)?;
        check!(
            served() == before + 1,
            "{} requests for hits",
            served() - before - 1
        );
        check!(first == again, "a hit returned other bytes");
        check!(near == device_bytes(69, 1), "a neighbour differs");
        check!(page == device_bytes(64, 8), "the page differs");
        // Another page is its own miss.
        read_back(disk, 72, 1)?;
        check!(served() == before + 2, "a new page was not one request");
        let (hits, misses) = provider::cache_counters(disk_id()).ok_or("no counters")?;
        check!(hits >= 3 && misses >= 2, "hits {hits} misses {misses}");
        Ok(())
    })();
    teardown();
    result
}

pub fn writes_go_through_and_update() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        write_pattern(disk, 0, 16, 2)?;
        read_back(disk, 0, 16)?; // both pages cached
        let before = served();
        // Overwrite a stretch across the page boundary.
        write_pattern(disk, 6, 4, 9)?;
        check!(served() == before + 1, "the write was not one request");
        check!(
            device_bytes(6, 4) == read_back(disk, 6, 4)?,
            "the device and the cache disagree after a write"
        );
        check!(
            served() == before + 1,
            "reading the written range went to the stick"
        );
        // Sectors around it kept their bytes.
        check!(
            device_bytes(0, 16) == read_back(disk, 0, 16)?,
            "a neighbour changed"
        );
        // Flush stays a request of its own.
        disk.flush().map_err(|e| format!("flush: {e:?}"))?;
        check!(with_fake(|fake| fake.flushes) == 1, "flush did not arrive");
        Ok(())
    })();
    teardown();
    result
}

pub fn failed_write_forgets() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        write_pattern(disk, 32, 8, 3)?;
        let old = read_back(disk, 32, 8)?;
        mode(Mode::Status(status::IO));
        let mut data = vec![0xEEu8; 8 * SECTOR_SIZE];
        data[0] = 0xAB;
        expect_err(
            disk.write_sectors(32, &data),
            BlockError::Io,
            "a failed write",
        )?;
        mode(Mode::Normal);
        // The stick kept the old bytes (the fake applies only OK writes); a
        // forgotten page is read again rather than served from memory.
        let before = served();
        let now = read_back(disk, 32, 8)?;
        check!(served() == before + 1, "the page was not forgotten");
        check!(
            now == old,
            "the read after a failed write returned new bytes"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn dead_disk_fails_even_when_cached() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        write_pattern(disk, 0, 8, 4)?;
        read_back(disk, 0, 8)?;
        mode(Mode::Silent);
        for _ in 0..provider::DEAD_AFTER_TIMEOUTS {
            let mut other = [0u8; SECTOR_SIZE];
            expect_err(
                disk.read_sectors(800, &mut other),
                BlockError::Io,
                "an uncached read from a silent provider",
            )?;
        }
        let mut cached = [0u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(0, &mut cached),
            BlockError::Io,
            "a cached read from a dead disk",
        )
    })();
    teardown();
    result
}

pub fn streams_bypass() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let sectors = 128; // 64 KiB: past the bypass size
        write_pattern(disk, 0, sectors, 5)?;
        let before = served();
        let first = read_back(disk, 0, sectors)?;
        let second = read_back(disk, 0, sectors)?;
        check!(first == second, "a stream read changed");
        check!(
            served() >= before + 2,
            "a stream was served from the cache ({} requests)",
            served() - before
        );
        let pages = provider::cache_pages(disk_id()).ok_or("no pages")?;
        check!(pages == 0, "a stream filled {pages} pages");
        Ok(())
    })();
    teardown();
    result
}

/// The cache itself: a stamp taken before a write cannot store what the
/// reader saw, an insert of whole pages only, eviction at the cap.
pub fn epoch_and_eviction() -> Result<(), String> {
    let mut cache = ReadCache::new();
    let page = vec![7u8; PAGE_SECTORS as usize * SECTOR_SIZE];
    // A reader's stamp, then a write begins and ends before it inserts.
    let stamp = cache.epoch();
    let ticket = cache.begin_write();
    cache.insert(stamp, 0, &page);
    check!(cache.len() == 0, "a stale insert was stored");
    cache.wrote(ticket, 0, &page[..SECTOR_SIZE]);
    cache.insert(stamp, 0, &page);
    check!(
        cache.len() == 0,
        "an insert before the write finished was stored"
    );
    // A fresh stamp stores; a misaligned or partial one does not.
    let stamp = cache.epoch();
    cache.insert(stamp, 1, &page);
    check!(cache.len() == 0, "an unaligned insert was stored");
    cache.insert(stamp, 0, &page[..page.len() - SECTOR_SIZE]);
    check!(cache.len() == 0, "a partial page was stored");
    cache.insert(stamp, 0, &page);
    check!(cache.len() == 1, "a whole page was not stored");
    let mut out = vec![0u8; SECTOR_SIZE];
    check!(cache.read(3, &mut out) && out == [7u8; SECTOR_SIZE], "read");
    // `wrote` updates, `forget` drops.
    let ticket = cache.begin_write();
    cache.wrote(ticket, 3, &[9u8; SECTOR_SIZE]);
    check!(
        cache.read(3, &mut out) && out == [9u8; SECTOR_SIZE],
        "wrote"
    );
    // Two writes in flight at once: whichever finishes first cannot tell
    // the device's final order, so neither may leave bytes in the cache.
    let first = cache.begin_write();
    let second = cache.begin_write();
    cache.wrote(first, 3, &[1u8; SECTOR_SIZE]);
    check!(
        !cache.read(3, &mut out),
        "an overlapping write left its bytes in the cache"
    );
    cache.wrote(second, 3, &[2u8; SECTOR_SIZE]);
    check!(
        !cache.read(3, &mut out),
        "the second overlapping write left its bytes in the cache"
    );
    cache.forget(0, 1);
    check!(cache.len() == 0, "forget kept the page");
    // Fill past the cap: it stays bounded and the newest page is there.
    let stamp = cache.epoch();
    for index in 0..700u64 {
        cache.insert(stamp, index * PAGE_SECTORS, &page);
    }
    check!(cache.len() <= 512, "{} pages held", cache.len());
    let mut sector = vec![0u8; SECTOR_SIZE];
    check!(
        cache.read(699 * PAGE_SECTORS, &mut sector),
        "the newest page was evicted"
    );
    Ok(())
}

/// Random reads and writes of 1..40 sectors anywhere on the stick: every
/// read equals what the device holds (write-through keeps them one), and the
/// cache stays within its bound.
pub fn soak_random_io() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..3000u32 {
            let sectors = 1 + (next() % 40) as usize;
            let lba = next() % (SECTORS - sectors as u64);
            if next() % 3 == 0 {
                write_pattern(disk, lba, sectors, round as u8)?;
            } else {
                let got = read_back(disk, lba, sectors)?;
                check!(
                    got == device_bytes(lba, sectors),
                    "round {round}: read {lba}+{sectors} differs from the device"
                );
            }
        }
        let pages = provider::cache_pages(disk_id()).ok_or("no pages")?;
        check!(pages <= 512, "{pages} pages held");
        let (hits, misses) = provider::cache_counters(disk_id()).ok_or("no counters")?;
        check!(hits > 0 && misses > 0, "hits {hits} misses {misses}");
        Ok(())
    })();
    teardown();
    result
}
