//! `Ext2::recover`: the only way a volume found unclean becomes clean again.

use super::ops::pattern;
use super::*;
use crate::{Owner, Recovery, ORPHAN_PREFIX};

/// `s_state`, straight from the bytes.
fn state(io: &MemIo) -> u8 {
    io.snapshot()[1024 + 0x3A]
}

/// A volume with a file on it that was never flushed: an unclean stop.
fn crashed() -> MemIo {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/keep", &pattern(9000), 0o644, 0, 0, 1)
        .unwrap();
    drop(fs);
    assert_eq!(state(&io) & 1, 0);
    io
}

#[test]
fn a_clean_volume_needs_nothing() {
    let (io, mut fs) = fresh(2 * 1024 * 1024, 4096);
    let before = io.snapshot();
    assert_eq!(fs.recover(ORPHAN_PREFIX), Ok(Recovery::WasClean));
    fs.flush().unwrap();
    assert_eq!(io.snapshot(), before, "recovering a clean volume wrote");
}

#[test]
fn without_recover_an_unclean_volume_stays_unclean() {
    let io = crashed();
    for _ in 0..3 {
        let fs = open(&io);
        assert!(!fs.was_clean_at_mount());
        fs.create("/x", 0o644, Owner::ROOT).unwrap();
        fs.unlink("/x").unwrap();
        fs.flush().unwrap();
        assert_eq!(state(&io) & 1, 0, "a clean shutdown laundered the volume");
    }
}

#[test]
fn a_consistent_unclean_volume_is_recovered() {
    let io = crashed();
    let mut fs = open(&io);
    assert_eq!(
        fs.recover(ORPHAN_PREFIX),
        Ok(Recovery::Recovered {
            reclaimed: 0,
            repairs: Default::default()
        })
    );
    fs.write_file("/after", b"more", 0o644, 0, 0, 1).unwrap();
    fs.flush().unwrap();
    assert_eq!(state(&io), 1);
    let fs = open(&io);
    assert!(fs.was_clean_at_mount());
    assert_eq!(fs.read_file("/keep").unwrap(), pattern(9000));
    assert_clean(&io);
}

#[test]
fn recovery_reclaims_orphans_first() {
    let io = crashed();
    let fs = open(&io);
    fs.write_file("/.unlinked-3", &pattern(20_000), 0o644, 0, 0, 1)
        .unwrap();
    drop(fs); // still unclean: the parked file is an orphan
    let mut fs = open(&io);
    assert_eq!(
        fs.recover(ORPHAN_PREFIX),
        Ok(Recovery::Recovered {
            reclaimed: 1,
            repairs: Default::default()
        })
    );
    fs.flush().unwrap();
    let fs = open(&io);
    assert!(fs.was_clean_at_mount());
    assert!(fs.lookup("/.unlinked-3").is_err(), "the orphan survived");
    assert_clean(&io);
}

/// The image build recovers through its block cache: the reclaim must reach
/// the disk (blocks and deferred frees) before the checker reads it raw, and
/// the result must be the same clean volume as the direct path's.
#[test]
fn a_cached_recovery_commits_the_reclaim_before_the_check() {
    let io = crashed();
    let fs = open(&io);
    fs.write_file("/.unlinked-7", &pattern(20_000), 0o644, 0, 0, 1)
        .unwrap();
    drop(fs);
    let mut fs = open_cached(&io, 256);
    assert_eq!(
        fs.recover(ORPHAN_PREFIX),
        Ok(Recovery::Recovered {
            reclaimed: 1,
            repairs: Default::default()
        })
    );
    // Before any flush, the disk already holds the reclaim, consistently.
    assert!(
        open(&io).lookup("/.unlinked-7").is_err(),
        "the reclaim is still cached"
    );
    assert_clean(&io);
    fs.write_file("/after", b"more", 0o644, 0, 0, 1).unwrap();
    fs.flush().unwrap();
    assert_eq!(state(&io), 1);
    let fs = open(&io);
    assert!(fs.was_clean_at_mount());
    assert_eq!(fs.read_file("/keep").unwrap(), pattern(9000));
    assert_clean(&io);
}

/// Damage no crash leaves (a reachable block marked free) is not repaired:
/// the volume stays flagged and nothing of the user's is removed.
#[test]
fn an_inconsistent_volume_stays_unclean() {
    let io = crashed();
    let fs = open(&io);
    let block = fs.mapped_block("/keep", 0).unwrap();
    super::repair::set_block_bit(&fs, block, false);
    drop(fs);
    let mut fs = open(&io);
    match fs.recover(ORPHAN_PREFIX).unwrap() {
        Recovery::StillUnclean(reason) => {
            assert!(
                reason.contains("fsck found") && reason.contains("not repaired"),
                "{reason}"
            )
        }
        other => panic!("an inconsistent volume was recovered: {other:?}"),
    }
    fs.flush().unwrap();
    assert_eq!(state(&io) & 1, 0, "an inconsistent volume was marked clean");
    assert_eq!(open(&io).read_file("/keep").unwrap(), pattern(9000));
}

/// What a crash does leave (here a lost free-block count) is repaired and
/// reported, and the volume is certified.
#[test]
fn crash_damage_is_repaired_and_reported() {
    let io = crashed();
    io.with_bytes(|bytes| bytes[1024 + 0x0C] = bytes[1024 + 0x0C].wrapping_sub(1));
    let mut fs = open(&io);
    match fs.recover(ORPHAN_PREFIX).unwrap() {
        Recovery::Recovered {
            reclaimed: 0,
            repairs,
        } => assert!(repairs.super_counters),
        other => panic!("not recovered: {other:?}"),
    }
    fs.flush().unwrap();
    assert_eq!(state(&io), 1);
    assert_clean(&io);
}

#[test]
fn recorded_errors_and_read_only_devices_are_left_alone() {
    let io = crashed();
    io.with_bytes(|bytes| bytes[1024 + 0x3A] = 2); // unclean + errors
    let mut fs = open(&io);
    assert!(matches!(
        fs.recover(ORPHAN_PREFIX),
        Ok(Recovery::StillUnclean(_))
    ));
    fs.flush().unwrap();
    assert_eq!(state(&io), 2);
    drop(fs);

    let io = crashed();
    io.set_writable(false);
    let mut fs = open(&io);
    assert!(matches!(
        fs.recover(ORPHAN_PREFIX),
        Ok(Recovery::StillUnclean(_))
    ));
}

/// Soak: 100 generations of a change and then a clean or an unclean stop. A
/// recover at every mount brings each unclean stop back, and the volume is
/// clean and intact at every following mount.
#[test]
fn soak_crash_recover_generations() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    drop(fs);
    let mut rng = 0x5EED_u32;
    for generation in 0..100u32 {
        let mut fs = open(&io);
        let outcome = fs.recover(ORPHAN_PREFIX).unwrap();
        assert!(
            matches!(outcome, Recovery::WasClean | Recovery::Recovered { .. }),
            "generation {generation}: {outcome:?}"
        );
        let body = pattern(1 + (generation as usize * 977) % 12_000);
        fs.write_file("/f", &body, 0o644, 0, 0, 1).unwrap();
        rng = rng.wrapping_mul(1_103_515_245).wrapping_add(12_345);
        if rng >> 16 & 1 == 0 {
            fs.flush().unwrap();
            assert_eq!(state(&io), 1, "generation {generation}");
        }
        drop(fs);
        assert_eq!(open(&io).read_file("/f").unwrap(), body);
    }
    assert_clean(&io);
}
