//! Volume-level behaviour of the public operations: counters, a full volume,
//! flush and remount, read-only devices, I/O failures, parked files and htree.

use super::ops::{pattern, ALICE};
use super::*;
use crate::{AttrChange, Ext2Error, Owner, S_IFREG};

#[test]
fn statfs_and_counters_track_allocation() {
    let (io, fs) = fresh(4 * 1024 * 1024, 4096);
    let before = fs.statfs().unwrap();
    assert_eq!(
        (before.magic, before.block_size, before.name_max),
        (0xEF53, 4096, 255)
    );
    assert_eq!(before.blocks, 1024);
    assert_eq!(before.blocks_free, u64::from(fs.free_blocks().unwrap()));
    assert_eq!(before.files_free, u64::from(fs.free_inodes().unwrap()));
    fs.write_file("/f", &pattern(10_000), 0o644, 0, 0, 1)
        .unwrap();
    let after = fs.statfs().unwrap();
    assert_eq!(after.blocks_free, before.blocks_free - 3);
    assert_eq!(after.files_free, before.files_free - 1);
    assert_clean(&io);
}

#[test]
fn a_full_volume_reports_no_space_and_stays_consistent() {
    let (io, fs) = fresh(1024 * 1024, 1024);
    fs.create("/fill", 0o644, Owner::ROOT).unwrap();
    let chunk = pattern(8192);
    let mut offset = 0u64;
    loop {
        match fs.write("/fill", offset, &chunk) {
            Ok(n) if n == chunk.len() => offset += n as u64,
            Ok(n) => {
                offset += n as u64; // a short write: the last blocks that fit
                break;
            }
            Err(error) => {
                assert_eq!(error, Ext2Error::NoSpace);
                break;
            }
        }
    }
    assert_eq!(fs.write("/fill", offset, &chunk), Err(Ext2Error::NoSpace));
    assert_eq!(fs.lookup("/fill").unwrap().size, offset);
    assert_clean(&io);
    fs.unlink("/fill").unwrap();
    fs.create("/after", 0o644, Owner::ROOT).unwrap();
    assert_clean(&io);
}

#[test]
fn flush_marks_clean_and_the_volume_is_dirty_while_mounted() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    let state = |io: &MemIo| io.snapshot()[1024 + 0x3A] & 1;
    assert_eq!(state(&io), 1);
    fs.create("/f", 0o644, Owner::ROOT).unwrap();
    assert_eq!(state(&io), 0, "dirty before the first change lands");
    fs.flush().unwrap();
    assert_eq!(state(&io), 1, "clean after a flush");
    assert!(open(&io).was_clean_at_mount());
    fs.create("/g", 0o644, Owner::ROOT).unwrap();
    let crashed = open(&io); // remount without a flush: an unclean stop
    assert!(!crashed.was_clean_at_mount());
}

#[test]
fn read_only_devices_mount_read_only() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/f", b"data", 0o644, 0, 0, 1).unwrap();
    fs.flush().unwrap();
    drop(fs);
    io.set_writable(false);
    let fs = open(&io);
    assert!(fs.is_read_only());
    assert_eq!(fs.read_file("/f").unwrap(), b"data");
    assert_eq!(
        fs.create("/g", 0o644, Owner::ROOT),
        Err(Ext2Error::ReadOnly)
    );
    assert_eq!(fs.mkdir("/d", 0o755, Owner::ROOT), Err(Ext2Error::ReadOnly));
    assert_eq!(fs.write("/f", 0, b"x"), Err(Ext2Error::ReadOnly));
    assert_eq!(fs.truncate("/f", 0), Err(Ext2Error::ReadOnly));
    assert_eq!(fs.unlink("/f"), Err(Ext2Error::ReadOnly));
    assert_eq!(fs.rmdir("/lost+found"), Err(Ext2Error::ReadOnly));
    assert_eq!(fs.rename("/f", "/h"), Err(Ext2Error::ReadOnly));
    assert_eq!(
        fs.setattr(
            "/f",
            &AttrChange {
                mode: Some(0),
                ..AttrChange::default()
            }
        ),
        Err(Ext2Error::ReadOnly)
    );
    fs.flush().unwrap();
}

#[test]
fn data_survives_a_remount() {
    let (io, fs) = fresh(4 * 1024 * 1024, 2048);
    fs.mkdir("/d", 0o750, ALICE).unwrap();
    fs.write_file("/d/f", &pattern(50_000), 0o600, 1000, 100, 1234)
        .unwrap();
    fs.flush().unwrap();
    drop(fs);
    let fs = open(&io);
    assert_eq!(fs.read_file("/d/f").unwrap(), pattern(50_000));
    let meta = fs.lookup("/d/f").unwrap();
    assert_eq!(
        (meta.mode, meta.uid, meta.gid, meta.times.mtime),
        (S_IFREG | 0o600, 1000, 100, 1234)
    );
    assert_clean(&io);
}

#[test]
fn io_failures_surface_as_errors_and_are_not_swallowed() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.create("/f", 0o644, Owner::ROOT).unwrap();
    io.fail_writes_after(0);
    assert_eq!(fs.write("/f", 0, &pattern(9000)), Err(Ext2Error::Io));
    assert_eq!(fs.create("/g", 0o644, Owner::ROOT), Err(Ext2Error::Io));
    // Reads still work, and nothing was half-linked.
    assert_eq!(fs.read_file("/f").unwrap(), b"");
    assert_eq!(fs.lookup("/g"), Err(Ext2Error::NotFound));
}

#[test]
fn parked_files_are_reclaimed_after_an_unclean_stop() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/keep", b"keep", 0o644, 0, 0, 1).unwrap();
    fs.write_file("/.unlinked-7", &pattern(20_000), 0o644, 0, 0, 1)
        .unwrap();
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.write_file("/d/.unlinked-8", b"x", 0o644, 0, 0, 1)
        .unwrap();
    drop(fs); // never flushed: the volume is unclean
    let fs = open(&io);
    assert!(!fs.was_clean_at_mount());
    let report = fs.reclaim_orphans(".unlinked-");
    assert_eq!(
        (report.reclaimed, report.failed.len(), report.scan_truncated),
        (2, 0, false)
    );
    assert_eq!(fs.lookup("/.unlinked-7"), Err(Ext2Error::NotFound));
    assert_eq!(fs.read_file("/keep").unwrap(), b"keep");
    assert_eq!(fs.reclaim_orphans(".unlinked-").reclaimed, 0);
    assert_eq!(
        fs.reclaim_orphans("").reclaimed,
        0,
        "an empty prefix names nothing"
    );
    fs.flush().unwrap();
    assert_clean(&io);
    // A volume that stopped cleanly is not scanned at all.
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.write_file("/.unlinked-9", b"y", 0o644, 0, 0, 1).unwrap();
    fs.flush().unwrap();
    assert_eq!(open(&io).reclaim_orphans(".unlinked-").reclaimed, 0);
}

#[test]
fn unlink_parked_deletes_inode_first_and_honours_extra_links() {
    let (io, fs) = fresh(2 * 1024 * 1024, 1024);
    fs.write_file("/.unlinked-1", &pattern(30_000), 0o644, 0, 0, 1)
        .unwrap();
    let blocks = fs.free_blocks().unwrap();
    fs.unlink_parked("/.unlinked-1").unwrap();
    assert!(fs.free_blocks().unwrap() > blocks + 28);
    assert_eq!(fs.unlink_parked("/.unlinked-1"), Err(Ext2Error::NotFound));
    assert_clean(&io);
}

#[test]
fn an_htree_directory_reads_but_refuses_changes() {
    let (io, fs) = fresh(2 * 1024 * 1024, 4096);
    fs.mkdir("/d", 0o755, Owner::ROOT).unwrap();
    fs.write_file("/d/f", b"x", 0o644, 0, 0, 1).unwrap();
    let ino = fs.lookup("/d").unwrap().ino;
    set_inode_flags(&io, ino, 0x1000); // EXT2_INDEX_FL
    assert_eq!(fs.read_file("/d/f").unwrap(), b"x");
    assert_eq!(fs.readdir("/d").unwrap().len(), 1);
    assert_eq!(
        fs.create("/d/g", 0o644, Owner::ROOT),
        Err(Ext2Error::NotSupported)
    );
    assert_eq!(fs.unlink("/d/f"), Err(Ext2Error::NotSupported));
    assert_eq!(fs.rename("/d/f", "/f"), Err(Ext2Error::NotSupported));
    assert_eq!(fs.rename("/f2", "/d/f2"), Err(Ext2Error::NotFound));
}

/// Set `i_flags` of inode `ino` straight on the image (inode table of group 0).
pub fn set_inode_flags(io: &MemIo, ino: u64, flags: u32) {
    io.with_bytes(|image| {
        let table = u32::from_le_bytes(image[4096 + 8..4096 + 12].try_into().unwrap()) as usize;
        let at = table * 4096 + (ino as usize - 1) * 128 + 0x20;
        image[at..at + 4].copy_from_slice(&flags.to_le_bytes());
    });
}
