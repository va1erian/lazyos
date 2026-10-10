//! Slow devices and stuck providers (issue #704): a stick that stalls a
//! request for many seconds is waited for, a provider that stops taking
//! requests still kills the disk within the short queue deadline, and many
//! slow requests in a row neither time out nor leak.

use super::*;

fn state() -> Result<(provider::Stats, bool), String> {
    provider::stats(with_fake(|fake| fake.disk)).ok_or_else(|| String::from("no stats"))
}

fn write_read(disk: &dyn BlockDevice, lba: u64, seed: u8) -> Result<(), String> {
    let mut sector = [0u8; SECTOR_SIZE];
    pattern(lba, seed, &mut sector);
    disk.write_sectors(lba, &sector)
        .map_err(|e| format!("write {lba}: {e:?}"))?;
    let mut back = [0u8; SECTOR_SIZE];
    disk.read_sectors(lba, &mut back)
        .map_err(|e| format!("read {lba}: {e:?}"))?;
    check!(back == sector, "sector {lba} read back other bytes");
    Ok(())
}

/// Every request stalls half the taken deadline (30 s, three times the
/// queue deadline): all of them complete, none times out.
pub fn slow_device_survives() -> Result<(), String> {
    let disk = setup(Mode::Slow(provider::TAKEN_TICKS / 2))?;
    let result = (|| {
        write_read(disk, 3, 1)?;
        disk.flush().map_err(|e| format!("slow flush: {e:?}"))?;
        let (stats, alive) = state()?;
        check!(
            alive && stats.timeouts == 0 && stats.errors == 0,
            "stats {stats:?} alive {alive}"
        );
        check!(
            test_clock::offset() >= 3 * provider::TAKEN_TICKS / 2,
            "the stall was not waited for ({} ticks)",
            test_clock::offset()
        );
        // Past the taken deadline the request fails, but one slow request
        // is not a dead disk, and its late answer is refused as stale.
        mode(Mode::Slow(
            provider::TAKEN_TICKS + 10 * provider::SLICE_TICKS,
        ));
        provider::drop_caches();
        let mut sector = [0u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(5, &mut sector),
            BlockError::Io,
            "a request past the taken deadline",
        )?;
        mode(Mode::Normal);
        write_read(disk, 7, 2)?;
        let (stats, alive) = state()?;
        check!(
            alive && stats.timeouts == 1,
            "stats {stats:?} alive {alive}"
        );
        check!(stats.stale >= 1, "the late answer was not stale: {stats:?}");
        Ok(())
    })();
    teardown();
    result
}

/// A provider that never takes a request is found out by the queue
/// deadline, not the long taken one, and the disk dies as before.
pub fn stuck_provider_dies_fast() -> Result<(), String> {
    let disk = setup(Mode::Absent)?;
    let result = (|| {
        let mut sector = [0u8; SECTOR_SIZE];
        for attempt in 0..provider::DEAD_AFTER_TIMEOUTS {
            let before = test_clock::offset();
            expect_err(
                disk.read_sectors(3, &mut sector),
                BlockError::Io,
                "a provider that never takes requests",
            )?;
            let waited = test_clock::offset() - before;
            check!(
                waited <= provider::QUEUE_TICKS + provider::SLICE_TICKS,
                "attempt {attempt} waited {waited} ticks"
            );
        }
        let (stats, alive) = state()?;
        check!(!alive, "the disk outlived a stuck provider");
        check!(
            with_fake(|fake| fake.served) == 0,
            "an untaken request was served"
        );
        check!(
            stats.timeouts == u64::from(provider::DEAD_AFTER_TIMEOUTS),
            "stats {stats:?}"
        );
        expect_err(
            disk.read_sectors(3, &mut sector),
            BlockError::Io,
            "a dead disk",
        )
    })();
    teardown();
    result
}

/// Two sticks on one provider: while it works on the other disk's request
/// (one transfer at a time), a request queued here waits past the queue
/// deadline without being counted, and the wait ends with the other
/// request's own deadline.
pub fn queued_behind_another_disk() -> Result<(), String> {
    let disk = setup(Mode::Absent)?;
    let result = (|| {
        let owner = with_fake(|fake| fake.owner);
        let other = provider::register(owner, SECTORS, true)
            .map_err(|e| format!("register a second disk: {e:?}"))?;
        test_clock::hold_taken(other);
        let before = test_clock::offset();
        let mut sector = [0u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(3, &mut sector),
            BlockError::Io,
            "a request behind a hung one",
        )?;
        let waited = test_clock::offset() - before;
        check!(
            waited >= provider::TAKEN_TICKS - provider::QUEUE_TICKS,
            "gave up after {waited} ticks while the provider was busy"
        );
        check!(
            waited <= provider::TAKEN_TICKS + provider::QUEUE_TICKS + 2 * provider::SLICE_TICKS,
            "waited {waited} ticks"
        );
        let (stats, alive) = state()?;
        check!(
            alive && stats.timeouts == 1,
            "stats {stats:?} alive {alive}"
        );
        Ok(())
    })();
    teardown();
    result
}

/// A provider that keeps taking fresh requests of another disk never takes
/// this one: the deferral ends after one taken request's worth, so the
/// request fails within `SLOT_TICKS` and counts once.
pub fn deferral_is_bounded() -> Result<(), String> {
    let disk = setup(Mode::Absent)?;
    let result = (|| {
        let owner = with_fake(|fake| fake.owner);
        let other = provider::register(owner, SECTORS, true)
            .map_err(|e| format!("register a second disk: {e:?}"))?;
        mode(Mode::BusyElsewhere(other));
        let before = test_clock::offset();
        let mut sector = [0u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(3, &mut sector),
            BlockError::Io,
            "a request behind an endlessly busy provider",
        )?;
        let waited = test_clock::offset() - before;
        check!(
            waited <= provider::TAKEN_TICKS + provider::QUEUE_TICKS + 2 * provider::SLICE_TICKS,
            "deferred for {waited} ticks"
        );
        check!(waited < provider::SLOT_TICKS, "waited {waited} ticks");
        let (stats, alive) = state()?;
        check!(
            alive && stats.timeouts == 1,
            "stats {stats:?} alive {alive}"
        );
        Ok(())
    })();
    teardown();
    result
}

/// Hundreds of requests, each stalled for a different long while (up to
/// 40 s), through the ext2-sized range: none times out, the data is right,
/// and the heap is where it started.
pub fn soak_slow_requests() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
        // The fake disk allocates a chunk on its first write: allocate them
        // all now, so the heap check sees only the request path.
        with_fake(|fake| {
            for at in (0..SECTORS as usize * SECTOR_SIZE).step_by(CHUNK) {
                fake.data.write(at, &[0]);
            }
        });
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut before = 0;
        for round in 0..300u32 {
            if round == 20 {
                before = measure();
            }
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            mode(Mode::Slow(seed % (4 * provider::QUEUE_TICKS)));
            write_read(disk, (seed >> 20) % SECTORS, round as u8)
                .map_err(|e| format!("round {round}: {e}"))?;
        }
        let (stats, alive) = state()?;
        check!(
            alive && stats.timeouts == 0 && stats.errors == 0 && stats.stale == 0,
            "stats {stats:?} alive {alive}"
        );
        provider::drop_caches();
        let after = measure();
        check!(
            after <= before + 4096,
            "heap grew from {before} to {after} bytes"
        );
        Ok(())
    })();
    teardown();
    result
}
