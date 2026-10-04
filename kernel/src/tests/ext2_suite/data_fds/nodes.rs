//! Descriptors read and write their file by inode (`fs/vfs/node.rs`,
//! docs/performance-plan.md P5): a name changed behind the open-file
//! registry's back (the native VFS renames and deletes without it) never
//! makes a descriptor read another file, and a new open of the name gets the
//! file that has it now.

use super::*;
use crate::fs::vfs::Filesystem;

pub fn names_changed_behind_the_registry() -> Result<(), String> {
    let data = Data::new(0)?;
    let fd = open("/data/n", O_CREAT | O_RDWR);
    check!(fd < 16, "create returned {fd:#x}");
    check!(write(fd, b"first file") == 10, "write failed");

    // Renamed on the volume directly: the descriptor keeps its file.
    data.fs.rename("n", "moved").map_err(fs_error)?;
    check!(
        pread(fd, 10, 0) == Ok(b"first file".to_vec()),
        "a rename behind the registry lost the file"
    );
    check!(pwrite(fd, b"F", 0) == 1, "write after the rename failed");
    check!(fstat_size(fd)? == 10, "fstat after the rename");

    // Deleted on the volume and its inode reused by a new file at the old
    // name: the descriptor must not read the new file's bytes.
    data.fs.unlink("moved").map_err(fs_error)?;
    data.fs.create("n", 0o644, Id::ROOT).map_err(fs_error)?;
    data.fs.write("n", 0, b"second").map_err(fs_error)?;
    check!(
        pread(fd, 6, 0) == Err(errno(ENOENT)),
        "a deleted file's descriptor read: {:?}",
        pread(fd, 6, 0)
    );
    check!(
        write(fd, b"x") == errno(ENOENT),
        "a deleted file's descriptor wrote into its reused inode"
    );
    check!(
        slurp("/data/n")? == b"second",
        "a new open of the name did not get the new file"
    );
    // The stale description lost the name: unlinking removes the new file
    // outright (nothing open has it), and the last close deletes nothing.
    check!(path_call(SYS_UNLINK, "/data/n", 0) == 0, "unlink failed");
    check!(close(fd) == 0, "close failed");
    check!(data_names()?.is_empty(), "names left: {:?}", data_names()?);
    data.check_clean()
}
