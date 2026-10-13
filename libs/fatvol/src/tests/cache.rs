//! The sector cache: coalescing, sticky failure, the clean flag, eviction.

use fused::daemon::FuseFs;

use super::*;
use crate::mem::MemDisk;
use crate::Fault;

/// Byte of the boot sector that holds the dirty bit (bit 0).
fn dirty_byte(fat: u8) -> usize {
    if fat == 32 {
        0x41
    } else {
        0x25
    }
}

#[test]
fn a_64k_write_costs_a_handful_of_device_requests() {
    let (mut vol, disk) = volume(32);
    disk.reset_counters();
    vol.create("f.bin", 0o644, 0, 0).unwrap();
    vol.write(path("f.bin"), 0, &pattern(64 * 1024, 1)).unwrap();
    let writes = disk.writes();
    // The data, the FAT sectors (both copies), the directory entry, the
    // dirty flag and FSInfo: far fewer than one request per FAT entry.
    assert!(writes <= 16, "{writes} device writes: {:?}", disk.write_log());
    let sectors: usize = disk.write_log().iter().map(|&(_, n)| n).sum();
    assert!(sectors >= 128, "the data itself is 128 sectors");
}

#[test]
fn nothing_waits_in_the_cache_after_an_operation() {
    let (mut vol, disk) = volume(16);
    put(&mut vol, "a.txt", &pattern(9000, 2)).unwrap();
    // No settle: the bytes are on the device already, through a second
    // volume opened over a copy of the image.
    let copy = MemDisk::from_image(&disk.image());
    let mut other = open(&copy, opts());
    assert_eq!(get(&mut other, "a.txt").unwrap(), pattern(9000, 2));
}

#[test]
fn the_dirty_flag_is_set_while_changing_and_cleared_when_idle() {
    for fat in [16, 32] {
        let (mut vol, disk) = volume(fat);
        assert_eq!(disk.byte(dirty_byte(fat)) & 1, 0);
        put(&mut vol, "x", b"x").unwrap();
        assert_eq!(disk.byte(dirty_byte(fat)) & 1, 1, "FAT{fat} dirty mid-use");
        let torn = MemDisk::from_image(&disk.image());
        assert!(open(&torn, opts()).dirty_at_open());
        vol.settle().unwrap();
        assert_eq!(disk.byte(dirty_byte(fat)) & 1, 0, "FAT{fat} clean when idle");
        let clean = MemDisk::from_image(&disk.image());
        assert!(!open(&clean, opts()).dirty_at_open());
        // The next request opens the volume again, and it works.
        assert_eq!(get(&mut vol, "x").unwrap(), b"x");
        put(&mut vol, "y", b"y").unwrap();
        assert_eq!(names(&mut vol, ""), ["x", "y"]);
    }
}

#[test]
fn settling_an_idle_volume_flushes_the_device() {
    let (mut vol, disk) = volume(16);
    put(&mut vol, "x", b"x").unwrap();
    let before = disk.flushes();
    vol.settle().unwrap();
    assert!(disk.flushes() > before);
}

#[test]
fn repeated_lookups_do_not_touch_the_device() {
    let (mut vol, disk) = volume(32);
    for i in 0..20 {
        put(&mut vol, &alloc::format!("file{i}.txt"), b"data").unwrap();
    }
    for _ in 0..3 {
        names(&mut vol, "");
    }
    disk.reset_counters();
    for round in 0..50 {
        let name = alloc::format!("file{}.txt", round % 20);
        vol.lookup(path(&name)).unwrap();
        names(&mut vol, "");
    }
    assert_eq!(disk.reads(), 0, "a warm directory is read from memory");
    assert_eq!(disk.writes(), 0, "looking changes nothing (no atime)");
}

#[test]
fn a_failed_write_is_sticky_and_names_the_fault() {
    let (mut vol, disk) = volume(16);
    put(&mut vol, "ok", b"ok").unwrap();
    disk.fail_writes_after(0);
    assert_errno(vol.create("bad", 0o644, 0, 0), e::EIO);
    assert_eq!(vol.failure(), Some(Fault::Io));
    // The volume does not guess what is on the medium: everything fails.
    assert_errno(vol.lookup(path("ok")), e::EIO);
    assert_errno(vol.create("again", 0o644, 0, 0), e::EIO);
}

#[test]
fn an_unplugged_device_ends_the_volume() {
    let (mut vol, disk) = volume(32);
    put(&mut vol, "ok", b"ok").unwrap();
    disk.unplug();
    // Whatever the cache still holds may be read; the first miss or write
    // finds the medium gone.
    let mut seen_gone = false;
    for i in 0..64 {
        let name = alloc::format!("new{i}");
        if vol.create(&name, 0o644, 0, 0).is_err() {
            seen_gone = true;
            break;
        }
    }
    assert!(seen_gone);
    assert_eq!(vol.failure(), Some(Fault::Gone));
}

#[test]
fn more_files_than_the_cache_holds_still_read_back() {
    let (mut vol, disk) = volume(32);
    vol.mkdir("many", 0o755, 0, 0).unwrap();
    // Directory entries and file data spread over far more than the
    // cache's pages, with flushes forced by eviction along the way.
    for i in 0..900 {
        let name = alloc::format!("many/file number {i:04}.dat");
        put(&mut vol, &name, &pattern(700 + i, i as u8)).unwrap();
    }
    vol.settle().unwrap();
    drop(vol);
    let mut vol = open(&disk, opts());
    assert_eq!(names(&mut vol, "many").len(), 900);
    for i in (0..900).step_by(37) {
        let name = alloc::format!("many/file number {i:04}.dat");
        assert_eq!(get(&mut vol, &name).unwrap(), pattern(700 + i, i as u8));
    }
}

/// Read a file until it ends (a torn file may end early), failing on errors.
fn read_to_end(vol: &mut Volume<MemDisk>, name: &str) {
    let mut at = 0;
    let mut buf = alloc::vec![0u8; 4096];
    loop {
        match vol.read(path(name), at, &mut buf) {
            Ok(0) => break,
            Ok(n) => at += n as u64,
            Err(code) => panic!("{name}: read at {at} failed with errno {code}"),
        }
    }
}

#[test]
fn a_cut_after_any_write_still_mounts() {
    // Replay one operation sequence, cutting the device after every number
    // of writes: whatever landed, the volume must open, list its root and
    // read what it lists to its end without an error or a hang. A file may
    // come back shorter than its entry says (FAT has no journal: a cut
    // between the chain and the entry is what `fsck.fat` repairs; the
    // checker's verdict is tools/fat/run.py's job), never unreadable.
    let script = |vol: &mut Volume<MemDisk>| {
        let _ = vol.mkdir("d", 0o755, 0, 0);
        let _ = put(vol, "d/one.txt", &pattern(20_000, 1));
        let _ = put(vol, "two.txt", &pattern(5_000, 2));
        let _ = vol.rename("two.txt", "d/two.txt");
        let _ = vol.truncate(path("d/one.txt"), 1000);
        let _ = vol.unlink("d/two.txt");
        let _ = put(vol, "three.txt", &pattern(40_000, 3));
    };
    let base = MemDisk::formatted(SECTORS, 16, LABEL);
    let total = {
        let disk = MemDisk::from_image(&base.image());
        let mut vol = open(&disk, opts());
        script(&mut vol);
        disk.writes()
    };
    assert!(total > 10);
    for cut in 0..=total {
        let disk = MemDisk::from_image(&base.image());
        disk.fail_writes_after(cut);
        {
            let mut vol = open(&disk, opts());
            script(&mut vol);
        }
        // Drop of the volume above may not write more after the cut.
        let survivor = MemDisk::from_image(&disk.image());
        let mut vol = open(&survivor, opts());
        for name in names(&mut vol, "") {
            if vol.lookup(path(&name)).unwrap().is_dir() {
                for inner in names(&mut vol, &name) {
                    read_to_end(&mut vol, &alloc::format!("{name}/{inner}"));
                }
            } else {
                read_to_end(&mut vol, &name);
            }
        }
    }
}

#[test]
fn a_failed_read_is_that_requests_error_alone() {
    let (mut vol, disk) = volume(16);
    put(&mut vol, "keep.txt", &pattern(3000, 7)).unwrap();
    vol.settle().unwrap();
    drop(vol);
    let mut vol = open(&disk, opts());
    vol.lookup(path("keep.txt")).unwrap(); // the directory is warm now
    // The file's data is cold: the read that has to go to the stick fails
    // once (a slow stick that timed out), and the next request is answered.
    disk.fail_next_reads(1);
    let mut buf = [0u8; 3000];
    assert_errno(vol.read(path("keep.txt"), 0, &mut buf), e::EIO);
    assert_eq!(vol.failure(), None);
    assert_eq!(get(&mut vol, "keep.txt").unwrap(), pattern(3000, 7));
    // Writing still works afterwards.
    put(&mut vol, "after.txt", b"ok").unwrap();
}

#[test]
fn reads_never_flush_the_device_and_a_read_only_mount_writes_nothing() {
    let (mut vol, disk) = volume(32);
    put(&mut vol, "r.txt", b"data").unwrap();
    vol.settle().unwrap();
    let flushes = disk.flushes();
    for _ in 0..5 {
        assert_eq!(get(&mut vol, "r.txt").unwrap(), b"data");
        vol.settle().unwrap();
    }
    assert_eq!(disk.flushes(), flushes, "idle reads sent a flush");
    // A write-protected stick: settling after reads must not touch it.
    disk.set_read_only(true);
    drop(vol);
    let mut vol = open(
        &disk,
        Options {
            writable: false,
            ..opts()
        },
    );
    get(&mut vol, "r.txt").unwrap();
    vol.settle().unwrap();
    assert_eq!(disk.flushes(), flushes);
}

#[test]
fn a_failed_read_with_changes_waiting_ends_the_volume() {
    // An operation half done must not be committed: a read failing while
    // sectors are dirty is as final as a failed write.
    let disk = MemDisk::formatted(SECTORS, 16, LABEL);
    let mut cache = crate::io::Cache::new(disk.clone(), true);
    cache.write_at(0, &[1u8; 512]).unwrap();
    disk.fail_next_reads(1);
    let mut buf = [0u8; 512];
    assert!(cache.read_at(8 * 512 * 40, &mut buf).is_err());
    assert_eq!(cache.failure(), Some(Fault::Io));
    assert!(cache.flush_dirty().is_err(), "the half-done change was committed");
    assert_eq!(disk.writes(), 0);
}
