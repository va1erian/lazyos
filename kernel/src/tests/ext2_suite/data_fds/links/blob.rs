//! WebKit's blob storage (`NetworkCache::BlobStorage`, issue #864) as it uses the file system, now that
//! `fallocate` lets the HTTP cache store large bodies as blobs: `add` writes the body to a file named by
//! its hash (the shared-mapping sequence of `filemap::cache_body`) and hard-links it under the record's
//! `-blob` name, `add` of the same body again links the blob that is there, `remove` unlinks one record's
//! name, and `synchronize` at the next start deletes every blob whose `st_nlink` is 1. The cache keeps
//! its bodies across a launch only if `link` works and `st_nlink` counts the names.

use super::filemap::cache_body::cache_sequence;
use super::*;

/// One run on ext2 and one on the ramfs; the ext2 one also goes through a remount.
pub fn blob_storage_cycle() -> Result<(), String> {
    let data = Data::new(0)?;
    cycle("/data", Some(&data))?;
    cycle("/tmp", None)?;
    data.check_clean()
}

fn cycle(dir: &str, remount: Option<&Data>) -> Result<(), String> {
    let body = filemap::pattern(3 * PAGE as usize + 123, 9);
    let blob = format!("{dir}/blob-hash");
    let first = format!("{dir}/record-1-blob");
    let second = format!("{dir}/record-2-blob");

    // add(): the body goes into the blob through a shared mapping, then the record's name is linked
    // (WebKit deletes a stale `-blob` name first: here there is none, so that is ENOENT).
    cache_sequence(&blob, &body, true)?;
    check!(
        path_call(SYS_UNLINK, &first, 0) == errno(ENOENT),
        "a record name exists before the blob is linked"
    );
    check!(link(&blob, &first) == 0, "link the blob under a record");
    let (ino, links) = ino_links(&blob)?;
    check!(links == 2, "{blob} has {links} links after the first add");
    check!(
        ino_links(&first)? == (ino, 2),
        "the record name is not the blob"
    );
    // The name must be free: a second link to it is refused, which is why `add` unlinks first.
    check!(
        link(&blob, &first) == errno(EEXIST),
        "linking over a record name"
    );

    // add() of the same body again: the blob exists and is equal, so it is linked, not rewritten.
    check!(
        link(&blob, &second) == 0,
        "link the blob under a second record"
    );
    check!(
        ino_links(&second)? == (ino, 3),
        "three names expected after the second add"
    );
    for path in [&blob, &first, &second] {
        check!(slurp(path)? == body, "{path} does not read back the body");
    }

    if let Some(data) = remount {
        check!(syscall(SYS_SYNC, 0, 0, 0, 0) == 0, "sync");
        data.remount()?;
        check!(
            ino_links(&blob)?.1 == 3,
            "{blob}: the link count did not survive a remount"
        );
    }

    // remove() of one record: the blob and the other record stay, and the count follows.
    check!(
        path_call(SYS_UNLINK, &first, 0) == 0,
        "unlink the first record"
    );
    check!(ino_links(&blob)?.1 == 2, "{blob} after the first remove");
    check!(slurp(&second)? == body, "the second record lost the body");

    // The cache keeps a blob mapped while a record is removed: the body stays readable through the map.
    let fd = filemap::open_ro(&blob)?;
    check!(fd_links(fd)? == 2, "fstat of the open blob");
    check!(
        path_call(SYS_UNLINK, &second, 0) == 0,
        "unlink the second record"
    );
    check!(
        fd_links(fd)? == 1 && ino_links(&blob)? == (ino, 1),
        "the blob must be down to one name"
    );
    check!(
        filemap::pread_ok(fd, body.len(), 0)? == body,
        "the open blob lost its body"
    );
    close(fd);

    // synchronize(): a blob with one link is nobody's body; it is deleted, and nothing is left behind.
    check!(
        path_call(SYS_UNLINK, &blob, 0) == 0,
        "synchronize deletes the blob"
    );
    for path in [&blob, &first, &second] {
        check!(
            path_call(SYS_UNLINK, path, 0) == errno(ENOENT),
            "{path} still exists"
        );
    }
    check!(parked_in(dir)? == 0, "an .unlinked-* name is left in {dir}");
    Ok(())
}
