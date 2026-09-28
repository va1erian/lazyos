//! Copy-up on first write, directory lifecycle (mkdir/rmdir/
//! whiteouts), and rename semantics against the lower layer.

use super::*;

/// Copy-up makes the first write land in the upper layer: reads fall
/// through, read-your-writes holds, metadata sizes track, and the lower
/// layer is byte-identical afterwards. Also covers `read_dir` union and
/// the unlink/whiteout cycle for lower and upper entries.
pub fn copy_up_read_write() -> Result<(), String> {
    let lower = lower_fixture()?;
    let (mut vfs, overlay) = mounted_overlay(lower.clone())?;
    let root = Id::ROOT;

    // Lower reads fall through unchanged.
    let meta = vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?;
    check!(
        meta.kind == FileKind::File && meta.size == 17,
        "lower meta is {meta:?}"
    );
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
        "lower read differs"
    );
    check!(
        overlay.usage() == (0, 1),
        "untouched overlay usage {:?}",
        overlay.usage()
    );

    // The first write copies the file up and changes only the upper copy.
    check!(
        vfs.write(root, "/HELLO.TXT", 6, b"aBI ")
            .map_err(fs_error)?
            == 4,
        "copy-up write was short"
    );
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello aBI  LazyOS",
        "read-your-writes failed"
    );
    check!(
        vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 17,
        "size after overwrite"
    );
    check!(
        lower_bytes(&lower, "HELLO.TXT")? == b"Hello from LazyOS",
        "copy-up modified the lower layer"
    );
    check!(
        overlay.usage().0 == 17,
        "copy-up usage {:?}",
        overlay.usage()
    );

    // A write past EOF zero-fills and grows the reported size.
    check!(
        vfs.write(root, "/HELLO.TXT", 20, b"!").map_err(fs_error)? == 1,
        "extending write was short"
    );
    check!(
        vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 21,
        "extended size"
    );
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello aBI  LazyOS\0\0\0!",
        "sparse extension bytes"
    );

    // Truncate shrinks the cached metadata and the data.
    vfs.truncate(root, "/HELLO.TXT", 5).map_err(fs_error)?;
    check!(
        vfs.stat(root, "/HELLO.TXT").map_err(fs_error)?.size == 5
            && vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello",
        "truncate did not shrink"
    );

    // A write deep in a lower-only tree copies the ancestor dirs up too.
    vfs.write(root, "/LDIR/INNER.TXT", 0, b"INNER")
        .map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/LDIR/INNER.TXT").map_err(fs_error)? == b"INNER",
        "nested copy-up read"
    );
    check!(
        lower_bytes(&lower, "LDIR/INNER.TXT")? == b"inner",
        "nested copy-up modified the lower layer"
    );

    // create + write + read-back, then the union listing.
    vfs.create(root, "/NEW.TXT", 0o644).map_err(fs_error)?;
    vfs.write(root, "/NEW.TXT", 0, b"new").map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/NEW.TXT").map_err(fs_error)? == b"new",
        "new file round-trip"
    );
    let listing = vfs.readdir(root, "/").map_err(fs_error)?;
    for expected in ["HELLO.TXT", "LOWER.TXT", "LDIR", "NEW.TXT"] {
        check!(
            has(&listing, expected),
            "readdir union missing {expected}: {:?}",
            names(&listing)
        );
    }

    // Unlinking an upper file removes it and leaves no whiteout behind.
    vfs.unlink(root, "/NEW.TXT").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/NEW.TXT").err() == Some(FsError::NotFound),
        "unlinked upper file still resolves"
    );
    check!(
        !has(&vfs.readdir(root, "/").map_err(fs_error)?, "NEW.TXT"),
        "readdir shows it"
    );
    let before = overlay.usage();

    // Unlinking a lower-only file hides it without touching the lower.
    vfs.unlink(root, "/LOWER.TXT").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/LOWER.TXT").err() == Some(FsError::NotFound),
        "whiteout did not hide the lower file"
    );
    check!(
        !has(&vfs.readdir(root, "/").map_err(fs_error)?, "LOWER.TXT"),
        "readdir still lists a whiteout"
    );
    check!(
        lower_bytes(&lower, "LOWER.TXT")? == b"lower",
        "whiteout modified the lower layer"
    );
    check!(
        overlay.usage().1 == before.1 + 1,
        "whiteout did not account a node: {:?} -> {:?}",
        before,
        overlay.usage()
    );

    // Re-creating the name clears the whiteout and shows the new bytes.
    vfs.create(root, "/LOWER.TXT", 0o644).map_err(fs_error)?;
    vfs.write(root, "/LOWER.TXT", 0, b"fresh")
        .map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/LOWER.TXT").map_err(fs_error)? == b"fresh",
        "re-created file shows stale bytes"
    );
    check!(
        has(&vfs.readdir(root, "/").map_err(fs_error)?, "LOWER.TXT"),
        "re-created file missing from readdir"
    );
    Ok(())
}

/// Directory lifecycle: nested `mkdir`, `rmdir` emptiness rules, whiteouts
/// for lower-only directories, and the errors the VFS classifies.
pub fn dir_create_remove() -> Result<(), String> {
    let lower = lower_fixture()?;
    let (mut vfs, _) = mounted_overlay(lower.clone())?;
    let root = Id::ROOT;

    // The fixture's `create_dir_all("ABIDIR/SUB")` shape.
    vfs.mkdir(root, "/ABIDIR", 0o755).map_err(fs_error)?;
    vfs.mkdir(root, "/ABIDIR/SUB", 0o755).map_err(fs_error)?;
    let sub = vfs.readdir(root, "/ABIDIR").map_err(fs_error)?;
    check!(names(&sub) == ["SUB"], "ABIDIR is {:?}", names(&sub));
    check!(
        vfs.mkdir(root, "/ABIDIR", 0o755).err() == Some(FsError::Exists),
        "mkdir over an existing dir"
    );
    check!(
        vfs.rmdir(root, "/ABIDIR").err() == Some(FsError::NotEmpty),
        "rmdir removed a non-empty dir"
    );
    vfs.create(root, "/ABIDIR/SUB/F.TXT", 0o644)
        .map_err(fs_error)?;
    check!(
        vfs.unlink(root, "/ABIDIR/SUB").err() == Some(FsError::IsDir),
        "unlink removed a dir"
    );
    check!(
        vfs.rmdir(root, "/ABIDIR/SUB").err() == Some(FsError::NotEmpty),
        "rmdir on the child dir"
    );
    vfs.unlink(root, "/ABIDIR/SUB/F.TXT").map_err(fs_error)?;
    vfs.rmdir(root, "/ABIDIR/SUB").map_err(fs_error)?;
    vfs.rmdir(root, "/ABIDIR").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/ABIDIR").err() == Some(FsError::NotFound),
        "removed dir still resolves"
    );

    // A lower-only directory is shadowed by a whiteout after its contents
    // are removed, and the lower layer keeps both entries.
    check!(
        vfs.rmdir(root, "/LDIR").err() == Some(FsError::NotEmpty),
        "rmdir of a lower dir with contents"
    );
    vfs.unlink(root, "/LDIR/INNER.TXT").map_err(fs_error)?;
    check!(
        vfs.rmdir(root, "/LDIR").map_err(fs_error).is_ok(),
        "rmdir of a lower dir after removing its contents"
    );
    check!(
        vfs.stat(root, "/LDIR").err() == Some(FsError::NotFound)
            && vfs.stat(root, "/LDIR/INNER.TXT").err() == Some(FsError::NotFound),
        "whiteouted lower dir still resolves"
    );
    check!(
        lower.lookup("LDIR").is_ok() && lower.lookup("LDIR/INNER.TXT").is_ok(),
        "whiteout modified the lower layer"
    );
    Ok(())
}

/// Rename semantics: move a lower file out and back, replace a lower file
/// with an upper one, and the file/dir type and emptiness errors.
pub fn rename_replace() -> Result<(), String> {
    let lower = lower_fixture()?;
    let (mut vfs, _) = mounted_overlay(lower.clone())?;
    let root = Id::ROOT;

    // The fixture's rename-out-and-back pattern.
    vfs.rename(root, "/HELLO.TXT", "/ABIREN.TXT")
        .map_err(fs_error)?;
    check!(
        vfs.stat(root, "/HELLO.TXT").err() == Some(FsError::NotFound),
        "rename left the lower source visible"
    );
    check!(
        vfs.read_file(root, "/ABIREN.TXT").map_err(fs_error)? == b"Hello from LazyOS",
        "rename lost the contents"
    );
    check!(
        !has(&vfs.readdir(root, "/").map_err(fs_error)?, "HELLO.TXT")
            && has(&vfs.readdir(root, "/").map_err(fs_error)?, "ABIREN.TXT"),
        "readdir disagrees with the rename"
    );
    vfs.rename(root, "/ABIREN.TXT", "/HELLO.TXT")
        .map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/HELLO.TXT").map_err(fs_error)? == b"Hello from LazyOS",
        "rename back lost the contents"
    );
    check!(
        lower_bytes(&lower, "HELLO.TXT")? == b"Hello from LazyOS",
        "rename modified the lower layer"
    );

    // An upper file replaces a lower file at the destination.
    vfs.create(root, "/TMP.TXT", 0o644).map_err(fs_error)?;
    vfs.write(root, "/TMP.TXT", 0, b"replacement")
        .map_err(fs_error)?;
    vfs.rename(root, "/TMP.TXT", "/LOWER.TXT")
        .map_err(fs_error)?;
    check!(
        vfs.read_file(root, "/LOWER.TXT").map_err(fs_error)? == b"replacement",
        "replace rename kept stale bytes"
    );
    check!(
        lower_bytes(&lower, "LOWER.TXT")? == b"lower",
        "replace rename modified the lower layer"
    );
    check!(
        vfs.readdir(root, "/")
            .map_err(fs_error)?
            .iter()
            .filter(|entry| entry.name == "LOWER.TXT")
            .count()
            == 1,
        "replace rename duplicated the destination"
    );

    // Type and emptiness rules.
    vfs.mkdir(root, "/DIR", 0o755).map_err(fs_error)?;
    vfs.create(root, "/FILE.TXT", 0o644).map_err(fs_error)?;
    check!(
        vfs.rename(root, "/FILE.TXT", "/DIR").err() == Some(FsError::IsDir),
        "file replaced a directory"
    );
    check!(
        vfs.rename(root, "/DIR", "/FILE.TXT").err() == Some(FsError::NotDir),
        "directory replaced a file"
    );
    vfs.mkdir(root, "/DIR2", 0o755).map_err(fs_error)?;
    vfs.create(root, "/DIR2/X.TXT", 0o644).map_err(fs_error)?;
    check!(
        vfs.rename(root, "/DIR", "/DIR2").err() == Some(FsError::NotEmpty),
        "directory replaced a non-empty directory"
    );
    vfs.mkdir(root, "/EMPTY", 0o755).map_err(fs_error)?;
    vfs.rename(root, "/EMPTY", "/DIR").map_err(fs_error)?;
    check!(
        vfs.stat(root, "/DIR").is_ok() && vfs.stat(root, "/EMPTY").err() == Some(FsError::NotFound),
        "empty-dir replace failed"
    );
    check!(
        vfs.rename(root, "/DIR", "/DIR/SUB").err() == Some(FsError::Invalid),
        "rename moved a directory into itself"
    );
    Ok(())
}
