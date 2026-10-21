//! The native mount table (what `pkgd` and the services use) keeps the
//! promises of a shared program image (`image.rs`).

use super::image::{program, start, text_bytes, DATA, PATH, TEXT};
use super::*;
use crate::fs::vfs::{FsError, Id};

/// The native table (what `pkgd` and the services use) keeps the same
/// promises: no write or truncate of a running program, an unlink or a
/// rename over it parks the running file, and a directory it was unlinked
/// from can still be removed.
pub fn native_table_protects_a_running_program() -> Result<(), String> {
    with_native(native_cases)
}

/// Run `case` with `/data` mounted in the native table too, then check the
/// volume.
fn with_native(case: fn() -> Result<(), String>) -> Result<(), String> {
    use crate::fs::vfs::{MountFlags, Vfs};
    let data = Data::new(0)?;
    let mut table = Vfs::new();
    let _ = table.mount(
        fhs::mount::ROOT,
        Arc::new(crate::fs::ramfs::RamFs::new()),
        MountFlags::default(),
    );
    let _ = table.mount(fhs::mount::DATA, data.fs.clone(), MountFlags::default());
    let previous = crate::fs::install_native_for_test(table);
    let outcome = case();
    crate::fs::restore_native_for_test(previous);
    outcome?;
    data.check_clean()
}

fn native_cases() -> Result<(), String> {
    let dir = "/data/app";
    let program_path = "/data/app/prog";
    check!(path_call(SYS_MKDIR, dir, 0o755) == 0, "mkdir");
    put(program_path, &program(1))?;
    let running = start(program_path)?;
    check!(
        matches!(
            crate::fs::vfs_write(Id::ROOT, program_path, 0, b"x"),
            Err(FsError::TextBusy)
        ),
        "a native write to a running program"
    );
    check!(
        matches!(
            crate::fs::vfs_truncate(Id::ROOT, program_path, 1),
            Err(FsError::TextBusy)
        ),
        "a native truncate of a running program"
    );

    // Replaced by a native rename.
    put("/data/app/prog.new", &program(2))?;
    crate::fs::vfs_rename(Id::ROOT, "/data/app/prog.new", program_path)
        .map_err(|e| format!("native rename over a running program: {e:?}"))?;
    check!(
        running.load(TEXT + 4 * PAGE, 16)? == text_bytes(4 * PAGE, 16, 1),
        "an untouched page changed after a native rename over the program"
    );
    let updated = start(program_path)?;
    check!(
        updated.load(TEXT + 4 * PAGE, 16)? == text_bytes(4 * PAGE, 16, 2),
        "the replacement did not run"
    );

    // Removed as `pkgd` removes an app: unlink the files, then the directory.
    crate::fs::vfs_unlink(Id::ROOT, program_path)
        .map_err(|e| format!("native unlink of a running program: {e:?}"))?;
    let parked: Vec<String> = crate::fs::vfs_readdir(Id::ROOT, dir)
        .map_err(fs_error)?
        .into_iter()
        .map(|entry| entry.name)
        .filter(|name| crate::fs::hidden::is_reserved(name))
        .collect();
    check!(
        parked.len() == 2,
        "both displaced programs should be parked: {parked:?}"
    );
    // The native table lists the parked entries; unlinking one by that name
    // changes nothing.
    for name in &parked {
        crate::fs::vfs_unlink(Id::ROOT, &format!("{dir}/{name}"))
            .map_err(|e| format!("unlinking a parked name: {e:?}"))?;
    }
    crate::fs::vfs_rmdir(Id::ROOT, dir)
        .map_err(|e| format!("rmdir of a directory a running program was unlinked from: {e:?}"))?;
    check!(
        updated.load(TEXT + 9 * PAGE, 16)? == text_bytes(9 * PAGE, 16, 2),
        "an untouched page changed after the native unlink"
    );
    check!(
        running.load(TEXT + 9 * PAGE, 16)? == text_bytes(9 * PAGE, 16, 1),
        "the displaced program lost a page"
    );
    drop((running, updated));
    let names = data_names()?;
    check!(
        names
            .iter()
            .all(|name| !crate::fs::hidden::is_reserved(name)),
        "hidden entries outlived the last instance: {names:?}"
    );
    Ok(())
}

/// A running program renamed away through the native table is still the
/// running program: its new name cannot be written and, unlinked, parks it.
pub fn native_rename_keeps_protection() -> Result<(), String> {
    with_native(|| {
        put("/data/moved-from", &program(1))?;
        let running = start("/data/moved-from")?;
        crate::fs::vfs_rename(Id::ROOT, "/data/moved-from", "/data/moved-to")
            .map_err(|e| format!("native rename of a running program: {e:?}"))?;
        check!(
            open("/data/moved-to", O_WRONLY) == errno(26),
            "the renamed program opened for writing"
        );
        check!(
            matches!(
                crate::fs::vfs_write(Id::ROOT, "/data/moved-to", 0, b"x"),
                Err(FsError::TextBusy)
            ),
            "a native write to the renamed program"
        );
        crate::fs::vfs_unlink(Id::ROOT, "/data/moved-to")
            .map_err(|e| format!("unlink of the renamed program: {e:?}"))?;
        check!(
            running.load(TEXT + 6 * PAGE, 16)? == text_bytes(6 * PAGE, 16, 1),
            "the renamed and unlinked program lost an untouched page"
        );
        Ok(())
    })
}
