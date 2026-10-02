//! Sustained load on the cache: a million block operations through a tiny
//! cache, and hundreds of mount/unmount cycles that must hand every frame
//! back.

use super::*;

/// Block operations (cache hits, misses and blocks written back) the first
/// soak drives through an eight-page cache.
const BLOCK_OPS: u64 = 1_000_000;
const FILES: u32 = 16;
const CYCLES: u32 = 300;

/// One random operation on file `index` of `model`: write somewhere (growing
/// it), read part of it back and compare, truncate, or delete it.
fn churn(vfs: &mut Vfs, rng: &mut Rng, model: &mut [Vec<u8>], index: u32) -> Result<(), String> {
    let path = format!("/s{index}");
    let file = &mut model[index as usize];
    match rng.below(8) {
        0..=3 => {
            let at = rng.below(file.len() as u32 + 4_000) as usize;
            let data = pattern_bytes(rng.next(), 1 + rng.below(12_000) as usize);
            if file.is_empty() {
                let _ = vfs.create(Id::ROOT, &path, 0o644);
            }
            let written = vfs
                .write(Id::ROOT, &path, at as u64, &data)
                .map_err(fs_error)?;
            check!(written == data.len(), "{path}: short write");
            if file.len() < at + data.len() {
                file.resize(at + data.len(), 0);
            }
            file[at..at + data.len()].copy_from_slice(&data);
        }
        4..=5 if !file.is_empty() => {
            let at = rng.below(file.len() as u32) as usize;
            let len = (1 + rng.below(16_000) as usize).min(file.len() - at);
            let mut back = vec![0u8; len];
            vfs.read(Id::ROOT, &path, at as u64, &mut back)
                .map_err(fs_error)?;
            check!(
                back == file[at..at + len],
                "{path}: bytes {at}+{len} differ"
            );
        }
        6 if !file.is_empty() => {
            let size = rng.below(file.len() as u32) as usize;
            vfs.truncate(Id::ROOT, &path, size as u64)
                .map_err(fs_error)?;
            file.truncate(size);
        }
        7 if !file.is_empty() => {
            vfs.unlink(Id::ROOT, &path).map_err(fs_error)?;
            file.clear();
        }
        _ => {}
    }
    Ok(())
}

/// Keep the model within the 2 MiB volume: drop the biggest file when the
/// model holds more than 1.2 MiB.
fn trim(vfs: &mut Vfs, model: &mut [Vec<u8>]) -> Result<(), String> {
    while model.iter().map(Vec::len).sum::<usize>() > 1_200_000 {
        let (index, _) = model
            .iter()
            .enumerate()
            .max_by_key(|(_, file)| file.len())
            .ok_or("an empty model")?;
        vfs.unlink(Id::ROOT, &format!("/s{index}"))
            .map_err(fs_error)?;
        model[index].clear();
    }
    Ok(())
}

fn verify(vfs: &mut Vfs, model: &[Vec<u8>]) -> Result<(), String> {
    for (index, file) in model.iter().enumerate() {
        let path = format!("/s{index}");
        if file.is_empty() {
            continue;
        }
        check!(read_file(vfs, &path)? == *file, "{path} differs");
    }
    Ok(())
}

/// A million block operations through eight pages, with syncs and periodic
/// writebacks mixed in: every read matches the model, and the volume's
/// counters agree with its bitmaps at the end and after a remount.
pub fn block_ops() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let (fs, mut vfs) = cached(disk, 8)?;
    let mut rng = Rng(0x9E37_79B9);
    let mut model = vec![Vec::new(); FILES as usize];
    let mut operations = 0u64;
    loop {
        let stats = fs.cache_stats().ok_or("no cache")?;
        if stats.hits + stats.misses + stats.written >= BLOCK_OPS {
            break;
        }
        let index = rng.below(FILES);
        churn(&mut vfs, &mut rng, &mut model, index)?;
        trim(&mut vfs, &mut model)?;
        operations += 1;
        match rng.below(64) {
            0 => fs.flush().map_err(fs_error)?,
            1 => vfs.writeback_all(rng.below(2) == 0).map_err(fs_error)?,
            _ => {}
        }
    }
    verify(&mut vfs, &model)?;
    fs.flush().map_err(fs_error)?;
    check_volume(disk, BLOCKS)?;
    let stats = fs.cache_stats().ok_or("no cache")?;
    serial_println!("TEST:bcache_soak_block_ops:INFO:{operations} file operations, {stats:?}");
    drop((fs, vfs));
    let (fs, mut vfs) = remount_disk(disk)?;
    verify(&mut vfs, &model)?;
    drop((fs, vfs));
    release(disk);
    Ok(())
}

/// Mount, churn, and unmount (a quarter of the time without a sync: the drop must
/// still commit) many times over; the data survives each remount and the
/// frame count comes back to where it started.
pub fn remount_cycles() -> Result<(), String> {
    task::register_kernel();
    let disk = formatted(0)?;
    let frames = mem::frame_stats().live();
    let mut rng = Rng(0x0BAD_CAFE);
    let mut model = vec![Vec::new(); FILES as usize];
    for cycle in 0..CYCLES {
        let pages = [4, 32, 256][rng.below(3) as usize];
        let (fs, mut vfs) = cached(disk, pages)?;
        if cycle % 10 == 0 {
            verify(&mut vfs, &model)?;
        }
        for _ in 0..1 + rng.below(12) {
            let index = rng.below(FILES);
            churn(&mut vfs, &mut rng, &mut model, index)?;
            trim(&mut vfs, &mut model)?;
        }
        if rng.below(4) != 0 {
            fs.flush().map_err(fs_error)?;
        }
        drop((fs, vfs));
        if cycle % 50 == 49 {
            check_volume(disk, BLOCKS).map_err(|e| format!("cycle {cycle}: {e}"))?;
        }
    }
    let (fs, mut vfs) = remount_disk(disk)?;
    verify(&mut vfs, &model)?;
    drop((fs, vfs));
    let after = mem::frame_stats().live();
    check!(
        after == frames,
        "frames leaked: {frames} before, {after} after"
    );
    release(disk);
    Ok(())
}
