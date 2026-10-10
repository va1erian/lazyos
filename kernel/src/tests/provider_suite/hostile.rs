//! A provider that fails, lies, stops answering or dies: every request ends
//! in a clean error within its deadline, and a dead disk fails at once.

use super::*;

fn read_one(disk: &dyn BlockDevice) -> Result<(), BlockError> {
    let mut sector = [0u8; SECTOR_SIZE];
    disk.read_sectors(3, &mut sector)
}

fn write_one(disk: &dyn BlockDevice) -> Result<(), BlockError> {
    disk.write_sectors(3, &[0x11u8; SECTOR_SIZE])
}

fn accept(_: &mut [u8]) -> Result<(), provider::ProviderError> {
    Ok(())
}

fn nothing(_: &[u8]) -> Result<(), provider::ProviderError> {
    Ok(())
}

fn state() -> Result<(provider::Stats, bool), String> {
    provider::stats(with_fake(|fake| fake.disk)).ok_or_else(|| String::from("no stats"))
}

/// The disk is dead: requests fail at once, without reaching the provider.
fn fails_fast(disk: &dyn BlockDevice) -> Result<(), String> {
    let served = with_fake(|fake| fake.served);
    let clock = test_clock::offset();
    expect_err(read_one(disk), BlockError::Io, "read on a dead disk")?;
    expect_err(
        write_one(disk),
        BlockError::ReadOnly,
        "write on a dead disk",
    )?;
    expect_err(disk.flush(), BlockError::Io, "flush on a dead disk")?;
    check!(!disk.is_writable(), "a dead disk reports writable");
    check!(
        with_fake(|fake| fake.served) == served,
        "a dead disk's request reached the provider"
    );
    check!(
        test_clock::offset() == clock,
        "a dead disk's request waited"
    );
    Ok(())
}

pub fn error_statuses() -> Result<(), String> {
    let disk = setup(Mode::Status(status::IO))?;
    let result = (|| {
        expect_err(read_one(disk), BlockError::Io, "IO status")?;
        mode(Mode::Status(status::READ_ONLY));
        expect_err(write_one(disk), BlockError::ReadOnly, "READ_ONLY status")?;
        mode(Mode::Status(0xDEAD_BEEF));
        expect_err(disk.flush(), BlockError::Io, "unknown status")?;
        let (stats, alive) = state()?;
        check!(
            alive && stats.errors == 3 && stats.timeouts == 0,
            "stats {stats:?} alive {alive}"
        );
        // Errors are per request: the disk still works.
        mode(Mode::Normal);
        check!(
            read_one(disk).is_ok() && write_one(disk).is_ok(),
            "the disk did not recover"
        );
        // A failed read never hands back the provider's bytes.
        mode(Mode::Status(status::IO));
        let mut sector = [0x77u8; SECTOR_SIZE];
        expect_err(
            disk.read_sectors(3, &mut sector),
            BlockError::Io,
            "IO status",
        )?;
        check!(
            sector.iter().all(|&b| b == 0x77),
            "a failed read changed the caller's buffer"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn wrong_tag_times_out() -> Result<(), String> {
    let disk = setup(Mode::WrongTag)?;
    let result = (|| {
        expect_err(read_one(disk), BlockError::Io, "a forged completion")?;
        let (stats, alive) = state()?;
        check!(alive, "one timeout killed the disk");
        check!(stats.timeouts == 1 && stats.stale >= 1, "stats {stats:?}");
        // The real clock runs too, so the fake one may cover less.
        let waited = test_clock::offset();
        let bound = provider::TAKEN_TICKS / 2..=provider::TAKEN_TICKS + provider::SLICE_TICKS;
        check!(bound.contains(&waited), "waited {waited} ticks");
        // A success in between resets the count of timeouts in a row.
        mode(Mode::Normal);
        check!(read_one(disk).is_ok(), "the disk did not recover");
        mode(Mode::WrongTag);
        expect_err(read_one(disk), BlockError::Io, "second forged completion")?;
        check!(
            state()?.1,
            "timeouts that were not in a row killed the disk"
        );
        Ok(())
    })();
    teardown();
    result
}

pub fn silent_driver_dies() -> Result<(), String> {
    let disk = setup(Mode::Silent)?;
    let result = (|| {
        for attempt in 0..provider::DEAD_AFTER_TIMEOUTS {
            expect_err(read_one(disk), BlockError::Io, "a silent provider")?;
            let alive = state()?.1;
            check!(
                alive == (attempt + 1 < provider::DEAD_AFTER_TIMEOUTS),
                "after {} timeouts alive={alive}",
                attempt + 1
            );
        }
        // The provider wakes up too late: its answer is stale, and the dead
        // disk stays dead.
        mode(Mode::Normal);
        let last = with_fake(|fake| fake.last).ok_or("no request")?;
        let owner = with_fake(|fake| fake.owner);
        let late = provider::complete(
            with_fake(|f| f.disk),
            owner,
            last.tag,
            status::OK,
            &mut accept,
        );
        check!(
            late == Err(provider::ProviderError::NotOwner),
            "late completion {late:?}"
        );
        fails_fast(disk)
    })();
    teardown();
    result
}

pub fn death_mid_request() -> Result<(), String> {
    let disk = setup(Mode::DieAfterTake)?;
    let result = (|| {
        expect_err(
            write_one(disk),
            BlockError::Io,
            "the provider died holding the request",
        )?;
        check!(
            test_clock::offset() < provider::SLICE_TICKS,
            "noticed after {} ticks",
            test_clock::offset()
        );
        check!(!state()?.1, "the disk outlived its provider");
        fails_fast(disk)
    })();
    teardown();
    result
}

/// The provider task exits without the teardown hook having run yet: the
/// waiting requester notices at its next slice and gives up.
pub fn dead_owner_detected() -> Result<(), String> {
    let disk = setup(Mode::Silent)?;
    let result = (|| {
        let owner = with_fake(|fake| fake.owner);
        task::harness::switch_current(task::KERNEL_TASK);
        task::harness::finish(owner, 0);
        expect_err(read_one(disk), BlockError::Io, "the provider exited")?;
        check!(
            test_clock::offset() <= provider::SLICE_TICKS,
            "noticed after {} ticks",
            test_clock::offset()
        );
        check!(!state()?.1, "the disk outlived its provider");
        // The real teardown arrives later and finds nothing left to do.
        provider::teardown_task(owner);
        fails_fast(disk)
    })();
    teardown();
    result
}

pub fn medium_gone() -> Result<(), String> {
    let disk = setup(Mode::Status(status::GONE))?;
    let result = (|| {
        expect_err(read_one(disk), BlockError::Io, "GONE")?;
        check!(!state()?.1, "GONE left the disk alive");
        fails_fast(disk)
    })();
    teardown();
    result?;
    // REMOVE: only the owner may, once; the disk then fails fast.
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let (id, owner) = with_fake(|fake| (fake.disk, fake.owner));
        check!(
            provider::remove(id, task::KERNEL_TASK) == Err(provider::ProviderError::NotOwner),
            "a stranger removed the disk"
        );
        check!(read_one(disk).is_ok(), "the disk does not work");
        provider::remove(id, owner).map_err(|e| format!("remove: {e:?}"))?;
        check!(
            provider::remove(id, owner) == Err(provider::ProviderError::NotOwner),
            "removed twice"
        );
        fails_fast(disk)
    })();
    teardown();
    result
}

pub fn stale_completion() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        let (id, owner) = with_fake(|fake| (fake.disk, fake.owner));
        check!(read_one(disk).is_ok(), "read");
        let last = with_fake(|fake| fake.last).ok_or("no request")?;
        // A replayed completion, a guessed one, and one for another disk.
        let replay = provider::complete(id, owner, last.tag, status::OK, &mut accept);
        check!(
            replay == Err(provider::ProviderError::Stale),
            "replay {replay:?}"
        );
        let guess = provider::complete(id, owner, last.tag + 1, status::OK, &mut accept);
        check!(
            guess == Err(provider::ProviderError::Stale),
            "guess {guess:?}"
        );
        let other = provider::complete(id + 1, owner, last.tag, status::OK, &mut accept);
        check!(
            other == Err(provider::ProviderError::NotOwner),
            "other disk {other:?}"
        );
        let far = provider::complete(usize::MAX, owner, last.tag, status::OK, &mut accept);
        check!(
            far == Err(provider::ProviderError::NotOwner),
            "no such disk {far:?}"
        );
        // Strangers can neither take nor complete requests.
        let stranger = task::KERNEL_TASK;
        check!(
            provider::next(id, stranger, 0, &mut nothing) == Err(provider::ProviderError::NotOwner),
            "a stranger took a request"
        );
        check!(
            provider::complete(id, stranger, last.tag, status::OK, &mut accept)
                == Err(provider::ProviderError::NotOwner),
            "a stranger completed a request"
        );
        // Tags carry the slot: no two disks' tags collide.
        check!(
            last.tag >> 56 == id as u64,
            "tag {:#x} for disk {id}",
            last.tag
        );
        let (stats, alive) = state()?;
        check!(
            alive && stats.stale == 2 && stats.errors == 0,
            "stats {stats:?}"
        );
        check!(read_one(disk).is_ok(), "read after hostile completions");
        // Bad geometry is refused.
        check!(
            provider::register(owner, 0, true) == Err(provider::ProviderError::Invalid),
            "0 sectors"
        );
        check!(
            provider::register(owner, u64::MAX, true) == Err(provider::ProviderError::Invalid),
            "overflowing size"
        );
        Ok(())
    })();
    teardown();
    result
}

/// A requester `SIGKILL`ed while parked mid-request (holding the request
/// slot, as a `/home` reader holds the volume's gate around it) is not ended
/// inside the kernel: the request completes, the slot is released, the next
/// request goes straight through, and the requester dies only once it is
/// back in user mode.
pub fn kill_mid_request_releases_slot() -> Result<(), String> {
    let disk = setup(Mode::Normal)?;
    let result = (|| {
        task::harness::switch_current(task::KERNEL_TASK);
        let requester = task::spawn_fork().map_err(|e| format!("spawn: {e}"))?;
        // Parked in a syscall, its saved frame is a kernel frame.
        let user_cs =
            task::harness::set_kernel_frame(requester).ok_or("the requester has no frame")?;
        mode(Mode::KillRequester(requester));
        task::harness::switch_current(requester);
        let read = read_one(disk);
        task::harness::switch_current(task::KERNEL_TASK);
        with_fake(|fake| fake.killed.take()).ok_or("the provider never killed the requester")??;
        check!(read.is_ok(), "the killed requester's read failed: {read:?}");
        check!(
            task::harness::state(requester) == Some(task::TaskState::Runnable),
            "the requester ended inside the kernel: {:?}",
            task::harness::state(requester)
        );
        check!(
            crate::task::signal::killed(requester),
            "the kill is no longer pending"
        );

        // The slot was released: another request needs no deadline to pass.
        let clock = test_clock::offset();
        check!(read_one(disk).is_ok(), "the request after the kill failed");
        check!(
            test_clock::offset() == clock,
            "the request after the kill waited for the slot"
        );
        let (stats, alive) = state()?;
        check!(
            alive && stats.errors == 0 && stats.timeouts == 0 && stats.requests == 2,
            "stats {stats:?} alive {alive}"
        );

        // Back in user mode, the requester dies with the kill's status.
        task::harness::set_frame_cs(requester, user_cs);
        check!(
            task::harness::resume_delivery(requester),
            "the killed requester was resumed in user mode"
        );
        check!(
            task::reap_child() == Some((requester, 128 + crate::task::signal::SIGKILL as u64)),
            "the requester did not exit with 128 + SIGKILL"
        );
        Ok(())
    })();
    crate::task::signal::harness::reset();
    teardown();
    result
}
