//! More of audit H6 (one filesystem mounted at two points, `/tmp` and
//! `/transient`): the name an open file is shown by, a rename through the
//! other alias, who holds a volume busy through either name, and a soak of
//! open / unlink / recreate through both.

use super::alias_mount::aliased_vfs;
use super::*;
use crate::fs::openfile::{self, OpenFile};
use crate::task::holders;

/// Run `case` with the aliased VFS installed as the ABI table.
fn with_aliased(case: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    let vfs = aliased_vfs()?;
    let previous = crate::fs::install_abi_for_test(vfs);
    let outcome = case();
    crate::fs::restore_abi_for_test(previous);
    outcome
}

/// A file keeps its registry name (`path`, canonical) apart from the name it is
/// shown by (`display_path`, the alias it was opened through), and a rename
/// through either alias updates both.
pub fn alias_names_follow_renames() -> Result<(), String> {
    with_aliased(|| {
        let root = Id::ROOT;
        let before = openfile::open_files();
        crate::fs::abi_mkdir(root, "/tmp/d", 0o755).map_err(fs_error)?;
        crate::fs::abi_create(root, "/tmp/d/f", 0o644).map_err(fs_error)?;
        crate::fs::abi_write(root, "/tmp/d/f", 0, b"data").map_err(fs_error)?;
        let file = OpenFile::open("/tmp/d/f", true, true, false).map_err(fs_error)?;
        check!(
            file.path() == "/transient/d/f" && file.display_path() == "/tmp/d/f",
            "key {:?}, shown {:?}",
            file.path(),
            file.display_path()
        );

        // The directory is renamed through the other alias: both names follow,
        // the shown one into the alias that did the renaming.
        crate::fs::abi_rename(root, "/transient/d", "/transient/e").map_err(fs_error)?;
        check!(
            file.path() == "/transient/e/f" && file.display_path() == "/transient/e/f",
            "after a rename through the other alias: key {:?}, shown {:?}",
            file.path(),
            file.display_path()
        );
        // Through the alias it is shown in, it moves within that alias.
        crate::fs::abi_rename(root, "/tmp/e", "/tmp/g").map_err(fs_error)?;
        check!(
            file.path() == "/transient/g/f" && file.display_path() == "/tmp/g/f",
            "after a rename through the shown alias: key {:?}, shown {:?}",
            file.path(),
            file.display_path()
        );
        crate::fs::abi_rename(root, "/tmp/g", "/tmp/h").map_err(fs_error)?;
        check!(
            file.path() == "/transient/h/f" && file.display_path() == "/tmp/h/f",
            "after a second rename: key {:?}, shown {:?}",
            file.path(),
            file.display_path()
        );
        let mut buf = [0u8; 8];
        let read = file.read_at(0, &mut buf).map_err(fs_error)?;
        check!(
            &buf[..read] == b"data",
            "the file moved under its descriptor"
        );

        // Unlinked, it is shown by its hidden name.
        crate::fs::abi_unlink(root, "/transient/h/f").map_err(fs_error)?;
        check!(
            file.display_path().contains(".unlinked") && file.display_path() == file.path(),
            "an unlinked file is shown as {:?}",
            file.display_path()
        );
        drop(file);
        crate::fs::abi_rmdir(root, "/tmp/h").map_err(fs_error)?;
        check!(
            openfile::open_files() == before,
            "the registry kept {} files",
            openfile::open_files() - before
        );
        Ok(())
    })
}

/// A volume named through one alias is busy for a file held through the other,
/// and the holder is reported by the name it holds the file by.
pub fn alias_holders_use_canonical_names() -> Result<(), String> {
    with_aliased(|| {
        let root = Id::ROOT;
        crate::fs::abi_create(root, "/tmp/held", 0o644).map_err(fs_error)?;
        let file = OpenFile::open("/tmp/held", true, false, false).map_err(fs_error)?;
        // A task of its own to hold it (the suite's kernel task has no table).
        crate::task::register_kernel();
        crate::task::harness::reset();
        crate::task::harness::switch_current(crate::task::KERNEL_TASK);
        let me = crate::task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
        crate::task::harness::switch_current(me);
        let fd = crate::task::fd_open(crate::task::Fd::Vfs { file })
            .ok_or_else(|| String::from("no descriptor"))?;
        for prefix in ["/transient", "/tmp", "/transient/held", "/tmp/held"] {
            let found: Vec<_> = holders::under(prefix)
                .into_iter()
                .filter(|holder| holder.pid == me && !holder.cwd)
                .collect();
            check!(
                found.len() == 1 && found[0].path == "/tmp/held",
                "holders under {prefix}: {found:?}"
            );
        }
        check!(
            holders::under("/transient/held2")
                .iter()
                .all(|h| h.pid != me || h.cwd),
            "a longer name matched"
        );
        // A shell sitting in the volume, through either name.
        crate::task::set_cwd("/tmp");
        let cwd = holders::under("/transient")
            .into_iter()
            .find(|holder| holder.pid == me && holder.cwd);
        crate::task::set_cwd("/");
        check!(
            cwd.as_ref().is_some_and(|holder| holder.path == "/tmp"),
            "a working directory in the other alias is not a holder: {cwd:?}"
        );
        check!(crate::task::fd_close(fd), "close");
        crate::task::harness::switch_current(crate::task::KERNEL_TASK);
        crate::task::harness::finish(me, 0);
        while crate::task::reap_child().is_some() {}
        crate::task::harness::reset();
        crate::fs::abi_unlink(root, "/tmp/held").map_err(fs_error)?;
        Ok(())
    })
}

/// Soak: 1000 rounds of open through one name, open through the other,
/// unlink, recreate the name and open it again leave the registry where it
/// started, and the old descriptors keep the old file throughout.
pub fn alias_open_unlink_recreate_soak() -> Result<(), String> {
    with_aliased(|| {
        let root = Id::ROOT;
        let before = openfile::open_files();
        for round in 0..1000u32 {
            let (first, second) = if round % 2 == 0 {
                ("/tmp/s", "/transient/s")
            } else {
                ("/transient/s", "/tmp/s")
            };
            crate::fs::abi_create(root, first, 0o644).map_err(fs_error)?;
            crate::fs::abi_write(root, first, 0, b"old").map_err(fs_error)?;
            let one = OpenFile::open(first, true, true, false).map_err(fs_error)?;
            let two = OpenFile::open(second, true, true, false).map_err(fs_error)?;
            check!(
                openfile::open_files() == before + 1,
                "round {round}: two names of one file are {} entries",
                openfile::open_files() - before
            );
            crate::fs::abi_unlink(root, second).map_err(fs_error)?;
            crate::fs::abi_create(root, first, 0o644).map_err(fs_error)?;
            crate::fs::abi_write(root, first, 0, b"new").map_err(fs_error)?;
            let three = OpenFile::open(second, true, true, false).map_err(fs_error)?;
            check!(
                openfile::open_files() == before + 2,
                "round {round}: the unlinked file and its successor are {} entries",
                openfile::open_files() - before
            );
            let mut buf = [0u8; 4];
            for (file, want) in [(&one, b"old"), (&two, b"old"), (&three, b"new")] {
                let read = file.read_at(0, &mut buf).map_err(fs_error)?;
                check!(
                    &buf[..read] == want,
                    "round {round}: a description read {:?}",
                    &buf[..read]
                );
            }
            drop((one, two, three));
            crate::fs::abi_unlink(root, first).map_err(fs_error)?;
            check!(
                openfile::open_files() == before,
                "round {round}: the registry kept {} files",
                openfile::open_files() - before
            );
        }
        Ok(())
    })
}
