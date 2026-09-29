//! Double/triple-indirect files and the size cap: boundary cases, contiguous
//! fills, refusal past the cap, and soaks that check nothing leaks.

use super::*;

/// The largest file size the driver grows to (`layout::MAX_FILE_SIZE`).
const CAP: u64 = 0x7FFF_FFFF;

/// Markers planted on both sides of every map boundary read back, the blocks
/// around them stay holes, and unlinking returns every table and data block.
/// Runs at all three block sizes, since the boundaries move with the block size.
pub fn double_indirect_boundaries() -> Result<(), String> {
    task::register_kernel();
    for block_size in [1024u32, 2048, 4096] {
        let total = (DISK_SECTORS * SECTOR_SIZE) as u32 / block_size;
        let (fs, mut vfs, disk) = mounted(block_size, total)?;
        let root = Id::ROOT;
        let baseline = fs.free_blocks().map_err(fs_error)?;
        let ptrs = block_size / 4;
        let double_end = 12 + ptrs + ptrs * ptrs; // first triple-indirect block
        let indices = [
            11,
            12,
            12 + ptrs - 1,     // last single-indirect block
            12 + ptrs,         // first double-indirect block
            12 + 2 * ptrs - 1, // last block of the first second-level table
            12 + 2 * ptrs,     // first block of the second table
            double_end - 1,    // last double-indirect block
            double_end,        // first triple-indirect block
        ];
        vfs.create(root, "/f", 0o644).map_err(fs_error)?;
        let mut planted = Vec::new();
        for index in indices {
            let offset = u64::from(index) * u64::from(block_size);
            let marker = pattern_bytes(index, 16);
            if offset + 16 > CAP {
                // 4 KiB blocks reach the size cap before the triple range.
                check!(
                    vfs.write(root, "/f", offset, &marker) == Err(FsError::NoSpace),
                    "{block_size}: block {index} is past the cap but was written"
                );
                continue;
            }
            check!(
                vfs.write(root, "/f", offset, &marker).map_err(fs_error)? == 16,
                "{block_size}: the write at block {index} was short"
            );
            planted.push((index, offset, marker));
        }
        for (index, offset, marker) in &planted {
            let mut back = [0u8; 16];
            vfs.read(root, "/f", *offset, &mut back).map_err(fs_error)?;
            check!(
                back[..] == marker[..],
                "{block_size}: block {index} read back wrong"
            );
            check!(
                fs.mapped_block("/f", *index).map_err(fs_error)? != 0,
                "{block_size}: block {index} is not mapped"
            );
            let neighbour = index - 1;
            if !planted.iter().any(|(other, ..)| *other == neighbour) {
                check!(
                    fs.mapped_block("/f", neighbour).map_err(fs_error)? == 0,
                    "{block_size}: hole {neighbour} was allocated"
                );
            }
        }
        vfs.unlink(root, "/f").map_err(fs_error)?;
        check!(
            fs.free_blocks().map_err(fs_error)? == baseline,
            "{block_size}: unlink leaked blocks"
        );
        check_volume(disk, total)?;
    }
    Ok(())
}

/// A contiguous file that crosses into the double-indirect range: whole-file
/// round trip, an overwrite across the boundary, a shrink back into the single
/// range, and an exact block accounting at each step.
pub fn double_indirect_contiguous_file() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    let mut body = pattern_bytes(4, 300 * 1024);
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    check!(
        vfs.write(root, "/f", 0, &body).map_err(fs_error)? == body.len(),
        "the 300 KiB write was short"
    );
    // 300 data blocks + single table + double table + one second-level table.
    check!(
        baseline - fs.free_blocks().map_err(fs_error)? == 303,
        "300 KiB should use 303 blocks"
    );
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == body,
        "the round trip differs"
    );

    let seam = 268 * 1024 - 8; // straddles the last single / first double block
    vfs.write(root, "/f", seam as u64, &[0xEE; 40])
        .map_err(fs_error)?;
    body[seam..seam + 40].fill(0xEE);
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == body,
        "the overwrite across the single/double seam differs"
    );

    vfs.truncate(root, "/f", 100 * 1024).map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/f").map_err(fs_error)? == body[..100 * 1024]
            && baseline - fs.free_blocks().map_err(fs_error)? == 101,
        "shrinking out of the double range kept the wrong blocks"
    );
    vfs.truncate(root, "/f", 290 * 1024).map_err(fs_error)?; // a hole up there
    vfs.write(root, "/f", 289 * 1024, b"z").map_err(fs_error)?;
    check!(
        fs.mapped_block("/f", 289).map_err(fs_error)? != 0
            && fs.mapped_block("/f", 288).map_err(fs_error)? == 0,
        "rewriting into a regrown double range mapped the wrong block"
    );
    vfs.unlink(root, "/f").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == baseline,
        "unlink of a double-indirect file leaked blocks"
    );
    check_volume(disk, 512)
}

/// The size cap: a write straddling it is short, one past it (or one whose
/// offset would wrap a 32-bit block index) is refused, and none of it disturbs
/// the file or the volume. Triple-indirect blocks are exercised on the way.
pub fn size_cap_is_enforced() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    vfs.create(root, "/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/f", 0, b"head").map_err(fs_error)?;

    check!(
        vfs.write(root, "/f", CAP - 4, b"0123456789")
            .map_err(fs_error)?
            == 4,
        "a write straddling the cap was not short"
    );
    check!(
        vfs.stat(root, "/f").map_err(fs_error)?.size == CAP,
        "the size after a capped write is wrong"
    );
    let mut tail = [0u8; 4];
    vfs.read(root, "/f", CAP - 4, &mut tail).map_err(fs_error)?;
    check!(&tail == b"0123", "the capped write landed wrong bytes");

    let used = baseline - fs.free_blocks().map_err(fs_error)?;
    for offset in [CAP, CAP + 1, 1 << 32, (1 << 32) + 5, u64::MAX - 1] {
        check!(
            vfs.write(root, "/f", offset, b"x") == Err(FsError::NoSpace),
            "a write at {offset:#x} was not refused"
        );
    }
    check!(
        baseline - fs.free_blocks().map_err(fs_error)? == used
            && vfs.stat(root, "/f").map_err(fs_error)?.size == CAP,
        "refused writes changed the file or the volume"
    );
    let mut head = [0u8; 4];
    vfs.read(root, "/f", 0, &mut head).map_err(fs_error)?;
    check!(&head == b"head", "a wrapped offset clobbered block 0");
    check!(
        vfs.read(root, "/f", u64::MAX, &mut head)
            .map_err(fs_error)?
            == 0,
        "a read at a huge offset was not EOF"
    );

    vfs.truncate(root, "/f", 70 * 1024 * 1024)
        .map_err(fs_error)?; // cut in triple range
    vfs.truncate(root, "/f", 0).map_err(fs_error)?;
    vfs.unlink(root, "/f").map_err(fs_error)?;
    check!(
        fs.free_blocks().map_err(fs_error)? == baseline,
        "the triple-indirect file leaked blocks"
    );
    check_volume(disk, 512)
}

/// Soak: 300 generations of two interleaved files taking random sparse writes
/// and truncates, checked against a byte-for-byte model and against the
/// bitmaps. Any leaked or cross-linked block shows up as a mismatch.
pub fn soak_write_truncate_unlink() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let (base_blocks, base_inodes) = (
        fs.free_blocks().map_err(fs_error)?,
        fs.free_inodes().map_err(fs_error)?,
    );
    let mut rng = Rng(0x2545_F491);
    for generation in 0..300u32 {
        let names = ["/a", "/b"];
        let mut models: [Vec<u8>; 2] = [Vec::new(), Vec::new()];
        for name in names {
            vfs.create(root, name, 0o644).map_err(fs_error)?;
        }
        for step in 0..6u32 {
            let which = rng.below(2) as usize;
            if rng.below(3) == 0 {
                let size = rng.below(models[which].len() as u32 + 50_000) as usize;
                vfs.truncate(root, names[which], size as u64)
                    .map_err(fs_error)?;
                models[which].resize(size, 0);
            } else {
                // Mostly small offsets; sometimes far out, into the double range.
                let offset = match rng.below(4) {
                    0 => 270 * 1024 + rng.below(20_000) as usize,
                    _ => rng.below(20_000) as usize,
                };
                let data = pattern_bytes(generation * 8 + step, 1 + rng.below(4000) as usize);
                let written = vfs
                    .write(root, names[which], offset as u64, &data)
                    .map_err(fs_error)?;
                check!(
                    written == data.len(),
                    "generation {generation}: short write"
                );
                if models[which].len() < offset + data.len() {
                    models[which].resize(offset + data.len(), 0);
                }
                models[which][offset..offset + data.len()].copy_from_slice(&data);
            }
        }
        for (name, model) in names.iter().zip(&models) {
            check!(
                vfs.read_file(root, name).map_err(fs_error)? == *model,
                "generation {generation}: {name} differs from the model"
            );
        }
        let first = rng.below(2) as usize;
        vfs.unlink(root, names[first]).map_err(fs_error)?;
        vfs.unlink(root, names[1 - first]).map_err(fs_error)?;
        check!(
            fs.free_blocks().map_err(fs_error)? == base_blocks
                && fs.free_inodes().map_err(fs_error)? == base_inodes,
            "generation {generation}: blocks or inodes leaked"
        );
        check_volume(disk, 512)?;
    }
    Ok(())
}

/// Soak: repeatedly fill the volume with one large file (the write runs dry
/// inside the double-indirect range), verify what landed, and free it by
/// alternating unlink and truncate-to-zero. Free counts must return to the
/// baseline every round, including when allocation failed mid-table.
pub fn soak_fill_and_free_large() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, 512)?;
    let root = Id::ROOT;
    let baseline = fs.free_blocks().map_err(fs_error)?;
    let body = pattern_bytes(9, 700 * 1024);
    for round in 0..8u32 {
        vfs.create(root, "/big", 0o644).map_err(fs_error)?;
        let written = vfs.write(root, "/big", 0, &body).map_err(fs_error)?;
        check!(
            written > 268 * 1024 && written < body.len() && written % 1024 == 0,
            "round {round}: a full volume gave a {written}-byte write"
        );
        check!(
            fs.free_blocks().map_err(fs_error)? == 0,
            "round {round}: the volume is not full"
        );
        check!(
            vfs.read_file(root, "/big").map_err(fs_error)? == body[..written],
            "round {round}: the filled file reads back wrong"
        );
        check_volume(disk, 512)?;
        if round % 2 == 0 {
            vfs.unlink(root, "/big").map_err(fs_error)?;
        } else {
            vfs.truncate(root, "/big", 0).map_err(fs_error)?;
            vfs.unlink(root, "/big").map_err(fs_error)?;
        }
        check!(
            fs.free_blocks().map_err(fs_error)? == baseline,
            "round {round}: freeing the big file leaked blocks"
        );
        check_volume(disk, 512)?;
    }
    Ok(())
}
