//! `fsync` of one file and a rename write only what they need
//! (`libs/ext2fs/src/cache/targeted.rs`): what other files left dirty stays
//! dirty, the volume stays marked dirty, and what was asked for is on the
//! disk. The library's host tests cut the power at every write; these prove
//! the kernel wiring (`Filesystem::flush_node`, `Node::flush`) and keep the
//! behaviour under a long mixed run.

use super::*;
use crate::fs::vfs::{Node, READ};

/// Dirty bytes of a file nobody syncs, in 1 KiB blocks.
const BULK_KB: usize = 150;

fn node_of(vfs: &mut Vfs, path: &str) -> Result<Node, String> {
    vfs.open_node(Id::ROOT, path, READ)
        .map_err(fs_error)?
        .ok_or_else(|| format!("{path}: no node"))
}

/// A node's `fsync` writes that file, not the others, and does not mark the
/// volume clean; a whole-volume flush then writes the rest.
pub fn fsync_file_leaves_other_files_dirty() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 512)?;
    write_file(&mut vfs, "/small", &pattern_bytes(1, 3_000))?;
    vfs.flush(Id::ROOT, "/small").map_err(fs_error)?;
    check!(raw_state(disk) & 1 == 1, "a whole flush marks the volume clean");

    write_file(&mut vfs, "/bulk", &pattern_bytes(2, BULK_KB * 1024))?;
    let grown = pattern_bytes(3, 7_000);
    vfs.write(Id::ROOT, "/small", 3_000, &grown).map_err(fs_error)?;
    let node = node_of(&mut vfs, "/small")?;
    node.flush().map_err(fs_error)?;

    check!(
        fs.dirty_blocks() >= BULK_KB * 9 / 10,
        "fsync of one file wrote the other's blocks ({} still dirty)",
        fs.dirty_blocks()
    );
    check!(
        raw_state(disk) & 1 == 0,
        "a file fsync marked the volume clean"
    );
    let (_, _, mut after) = reboot_copy(disk)?;
    let mut expect = pattern_bytes(1, 3_000);
    expect.extend_from_slice(&grown);
    check!(
        read_file(&mut after, "/small")? == expect,
        "the synced file is not on the disk"
    );
    drop(after);

    vfs.flush(Id::ROOT, "/bulk").map_err(fs_error)?;
    check!(fs.dirty_blocks() == 0, "the whole flush left blocks dirty");
    check!(raw_state(disk) & 1 == 1, "the whole flush marks it clean");
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    release(disk);
    release(pooled(1));
    Ok(())
}

/// A rename orders itself without writing the other files' dirty blocks, and
/// the file is on the disk under one name afterwards.
pub fn rename_leaves_other_files_dirty() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 512)?;
    let data = pattern_bytes(5, 9_000);
    vfs.mkdir(Id::ROOT, "/d", 0o755).map_err(fs_error)?;
    write_file(&mut vfs, "/d/tmp", &data)?;
    vfs.flush(Id::ROOT, "/d").map_err(fs_error)?;

    write_file(&mut vfs, "/bulk", &pattern_bytes(6, BULK_KB * 1024))?;
    vfs.rename(Id::ROOT, "/d/tmp", "/d/final").map_err(fs_error)?;
    check!(
        fs.dirty_blocks() >= BULK_KB * 9 / 10,
        "the rename wrote the other file's blocks ({} still dirty)",
        fs.dirty_blocks()
    );
    let (_, _, mut after) = reboot_copy(disk)?;
    let found = ["/d/tmp", "/d/final"]
        .iter()
        .any(|path| read_file(&mut after, path).is_ok_and(|bytes| bytes == data));
    check!(found, "the renamed file is under neither name");
    drop(after);
    vfs.flush(Id::ROOT, "/d").map_err(fs_error)?;
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    release(disk);
    release(pooled(1));
    Ok(())
}

/// Soak: file fsyncs interleaved with appends, overwrites and large
/// rewrites. Every few rounds the disk is copied as a power cut would leave
/// it, and each file synced since its last change reads back exactly.
pub fn soak_fsync_beside_writers() -> Result<(), String> {
    task::register_kernel();
    const SMALL: usize = 8;
    const BULKS: usize = 3;
    const ROUNDS: u32 = 700;
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 512)?;
    let mut rng = Rng(0xC0FF_EE11);
    let mut content: Vec<Vec<u8>> = Vec::new();
    let mut synced: Vec<bool> = Vec::new();
    for n in 0..SMALL + BULKS {
        let data = pattern_bytes(n as u32, 500 + n * 300);
        write_file(&mut vfs, &format!("/f{n}"), &data)?;
        content.push(data);
        synced.push(false);
    }
    // Names durable: only content is judged below.
    vfs.flush(Id::ROOT, "/").map_err(fs_error)?;
    let mut syncs = 0u32;
    for round in 0..ROUNDS {
        let n = rng.next() as usize % (SMALL + BULKS);
        let path = format!("/f{n}");
        match rng.next() % 8 {
            0 | 1 => {
                let more = pattern_bytes(round, 1 + rng.next() as usize % 3_000);
                if content[n].len() < if n < SMALL { 20_000 } else { 60_000 } {
                    vfs.write(Id::ROOT, &path, content[n].len() as u64, &more)
                        .map_err(fs_error)?;
                    content[n].extend_from_slice(&more);
                    synced[n] = false;
                }
            }
            2 => {
                let at = rng.next() as usize % content[n].len();
                let patch = pattern_bytes(round ^ 0x55, 1 + rng.next() as usize % 200);
                let end = (at + patch.len()).min(content[n].len());
                vfs.write(Id::ROOT, &path, at as u64, &patch[..end - at])
                    .map_err(fs_error)?;
                content[n][at..end].copy_from_slice(&patch[..end - at]);
                synced[n] = false;
            }
            3 if n >= SMALL => {
                // A large rewrite: shrink to nothing and fill again.
                vfs.truncate(Id::ROOT, &path, 0).map_err(fs_error)?;
                let data = pattern_bytes(round, 20_000 + rng.next() as usize % 20_000);
                vfs.write(Id::ROOT, &path, 0, &data).map_err(fs_error)?;
                content[n] = data;
                synced[n] = false;
            }
            _ => {
                node_of(&mut vfs, &path)?.flush().map_err(fs_error)?;
                synced[n] = true;
                syncs += 1;
            }
        }
        if round % 50 == 49 {
            let (_, _, mut after) = reboot_copy(disk)?;
            for m in (0..SMALL + BULKS).filter(|&m| synced[m]) {
                check!(
                    read_file(&mut after, &format!("/f{m}"))? == content[m],
                    "round {round}: /f{m} was synced and is not on the disk"
                );
            }
        }
    }
    check!(syncs > 100, "the soak synced only {syncs} times");
    vfs.flush(Id::ROOT, "/").map_err(fs_error)?;
    check_volume(disk, BLOCKS)?;
    drop((fs, vfs));
    release(disk);
    release(pooled(1));
    Ok(())
}
