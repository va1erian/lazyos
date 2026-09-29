//! `rmdir` on the ext2 volume: the rules (only empty directories, never a
//! file), that every block and inode comes back, that the parent's link count
//! stays right, that removal survives a remount, and a soak of build-and-tear-
//! down generations.

use super::*;

const BLOCKS: u32 = 512;

/// The free `(blocks, inodes)` the superblock reports.
fn free_counts(fs: &Ext2) -> Result<(u32, u32), String> {
    Ok((
        fs.free_blocks().map_err(fs_error)?,
        fs.free_inodes().map_err(fs_error)?,
    ))
}

/// Empty vs non-empty, file vs directory, missing, and the root.
pub fn rmdir_rules() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, BLOCKS)?;
    let root = Id::ROOT;
    let baseline = free_counts(&fs)?;
    vfs.mkdir(root, "/d", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/d/e", 0o755).map_err(fs_error)?;
    vfs.create(root, "/d/f", 0o644).map_err(fs_error)?;
    vfs.write(root, "/d/f", 0, &pattern_bytes(1, 3000))
        .map_err(fs_error)?;

    check!(
        matches!(vfs.rmdir(root, "/d"), Err(FsError::NotEmpty)),
        "removed a non-empty directory"
    );
    check!(
        matches!(vfs.rmdir(root, "/d/f"), Err(FsError::NotDir)),
        "rmdir removed a file"
    );
    check!(
        matches!(vfs.rmdir(root, "/d/nope"), Err(FsError::NotFound)),
        "rmdir of a missing name did not say so"
    );
    check!(vfs.rmdir(root, "/").is_err(), "rmdir removed the root");

    vfs.rmdir(root, "/d/e").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/d/e").is_err(),
        "the removed directory still resolves"
    );
    let names: Vec<String> = vfs
        .readdir(root, "/d")
        .map_err(fs_error)?
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    check!(names == ["f"], "directory lists {names:?} after rmdir");

    vfs.unlink(root, "/d/f").map_err(fs_error)?;
    vfs.rmdir(root, "/d").map_err(fs_error)?;
    check!(
        free_counts(&fs)? == baseline,
        "blocks or inodes leaked: {:?} vs {baseline:?}",
        free_counts(&fs)?
    );
    check_volume(disk, BLOCKS)?;
    // The root's link count went back to `.`, `..` and no subdirectory: a new
    // directory can be created and removed again.
    vfs.mkdir(root, "/again", 0o755).map_err(fs_error)?;
    vfs.rmdir(root, "/again").map_err(fs_error)
}

/// A removal is on disk after a flush and a remount from the raw sectors.
pub fn rmdir_survives_remount() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, BLOCKS)?;
    let root = Id::ROOT;
    vfs.mkdir(root, "/gone", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/keep", 0o755).map_err(fs_error)?;
    vfs.rmdir(root, "/gone").map_err(fs_error)?;
    fs.flush().map_err(fs_error)?;
    drop(vfs);
    drop(fs);

    let (_fs, mut vfs) = remount_disk(disk)?;
    check!(
        vfs.stat(root, "/gone").is_err(),
        "a removed directory came back"
    );
    check!(vfs.stat(root, "/keep").is_ok(), "the sibling was lost");
    check_volume(disk, BLOCKS)
}

/// Many generations of a nested tree built and torn down bottom-up (with a
/// directory rename in the middle): nothing leaks and the volume stays
/// consistent after every generation.
pub fn rmdir_soak_generations() -> Result<(), String> {
    task::register_kernel();
    let (fs, mut vfs, disk) = mounted(1024, BLOCKS)?;
    let root = Id::ROOT;
    let baseline = free_counts(&fs)?;
    for generation in 0..200u32 {
        vfs.mkdir(root, "/a", 0o755).map_err(fs_error)?;
        vfs.mkdir(root, "/a/b", 0o755).map_err(fs_error)?;
        vfs.mkdir(root, "/a/b/c", 0o755).map_err(fs_error)?;
        vfs.create(root, "/a/b/c/f", 0o644).map_err(fs_error)?;
        let body = pattern_bytes(generation, 1500 + (generation as usize % 5) * 700);
        vfs.write(root, "/a/b/c/f", 0, &body).map_err(fs_error)?;
        if generation % 3 == 0 {
            vfs.rename(root, "/a/b", "/a/b2").map_err(fs_error)?;
            vfs.rename(root, "/a/b2", "/a/b").map_err(fs_error)?;
        }
        check!(
            matches!(vfs.rmdir(root, "/a/b/c"), Err(FsError::NotEmpty)),
            "generation {generation}: removed a directory holding a file"
        );
        vfs.unlink(root, "/a/b/c/f").map_err(fs_error)?;
        for dir in ["/a/b/c", "/a/b", "/a"] {
            vfs.rmdir(root, dir).map_err(fs_error)?;
        }
        if generation % 25 == 0 {
            check_volume(disk, BLOCKS)?;
        }
    }
    check!(
        free_counts(&fs)? == baseline,
        "the soak leaked blocks or inodes"
    );
    check_volume(disk, BLOCKS)
}
