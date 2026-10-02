//! The shutdown path on the configured layout: `/` is the ext2 OS volume,
//! shared by the native and the Linux ABI tables, and the power-off sync
//! (`power::sync_filesystems` -> `fs::sync_all`) runs over the native table.
//! A clean stop must leave the volume clean for the next boot whichever table
//! wrote to it, and a volume that once stopped uncleanly stays flagged across
//! clean stops until something checks it (the image build's `Ext2::recover`,
//! simulated here by restoring the clean bit).

use super::*;
use crate::block::BlockDevice;
use crate::fs::mounts::{self, Tables};
use crate::fs::vfs::{FsError, Id, Vfs};

fn fs_error(error: FsError) -> String {
    format!("{} ({error:?})", error.message())
}

/// `s_state` of the rig's root volume as stored right now.
fn root_state(rig: &Rig) -> u16 {
    let data = rig.root.data.lock();
    u16::from_le_bytes([data[SUPER + 0x3A], data[SUPER + 0x3B]])
}

/// What a passing check of the root leaves on disk: the clean bit.
fn check_root(rig: &Rig) {
    rig.root.data.lock()[SUPER + 0x3A] = 1;
}

/// The rig with `lazyos.cfg` naming its root, and that root freshly formatted.
fn fresh_rig() -> &'static Rig {
    let rig = rig(Some(&format!("root=UUID={}\n", uuid(0xA1).1)));
    rig.root
        .data
        .lock()
        .copy_from_slice(&ext2_image(uuid(0xA1).0, "rootfs"));
    rig
}

/// Boot: build both tables from the rig's devices, as `fs::init` does.
fn boot(rig: &Rig) -> Result<Tables, String> {
    let devices: [&'static dyn BlockDevice; 2] = [rig.boot, rig.root];
    let tables = mounts::build(&devices);
    check!(tables.mounted, "no root mounted");
    check!(
        tables.native.mount_fs_name("/") == Some("ext2 (rw)"),
        "/ is {:?}, not the ext2 OS volume",
        tables.native.mount_fs_name("/")
    );
    Ok(tables)
}

/// Replace `path` with `data` through `table`.
fn put(table: &mut Vfs, path: &str, data: &[u8]) -> Result<(), String> {
    let id = Id::ROOT;
    if table.stat(id, path).is_ok() {
        table.unlink(id, path).map_err(fs_error)?;
    }
    table.create(id, path, 0o644).map_err(fs_error)?;
    table.write(id, path, 0, data).map_err(fs_error)?;
    Ok(())
}

/// Correctness: writes through either table dirty the root at once, the
/// native table's `sync_all` (the power-off sync) marks it clean, and the next
/// boot finds it clean with both files intact. A sync with nothing changed
/// writes nothing.
pub fn root_power_cycle_marks_clean() -> Result<(), String> {
    task::register_kernel();
    let rig = fresh_rig();
    let mut tables = boot(rig)?;
    check!(root_state(rig) == 1, "a fresh root is not clean");
    let untouched = rig.root.data.lock().clone();
    tables.native.sync_all().map_err(fs_error)?;
    check!(
        *rig.root.data.lock() == untouched,
        "syncing an untouched root wrote to it"
    );

    put(&mut tables.abi, "/abi.txt", b"from a Linux program")?;
    check!(root_state(rig) & 1 == 0, "an ABI write left the root clean");
    put(&mut tables.native, "/native.txt", b"from a native service")?;
    tables.native.sync_all().map_err(fs_error)?;
    check!(
        root_state(rig) == 1,
        "the power-off sync left the root dirty"
    );
    drop(tables);

    let mut tables = boot(rig)?;
    check!(root_state(rig) == 1, "the next boot found the root dirty");
    let id = Id::ROOT;
    let native = tables.abi.read_file(id, "/native.txt").map_err(fs_error)?;
    let abi = tables.native.read_file(id, "/abi.txt").map_err(fs_error)?;
    check!(
        native == b"from a native service" && abi == b"from a Linux program",
        "a file did not survive the power cycle"
    );
    Ok(())
}

/// An unclean stop is seen by the next boot, and clean stops after it keep the
/// root flagged (the kernel has no fsck and must not launder it). Once checked
/// the root is clean at the next boot and stays clean across clean stops.
pub fn unclean_root_stays_flagged_until_checked() -> Result<(), String> {
    task::register_kernel();
    let rig = fresh_rig();
    let mut tables = boot(rig)?;
    put(&mut tables.native, "/f", b"lost power")?;
    drop(tables); // no sync: the window was closed
    for stop in 0..3 {
        let mut tables = boot(rig)?;
        check!(
            root_state(rig) & 1 == 0,
            "stop {stop}: an unclean root was seen as clean"
        );
        put(&mut tables.abi, "/f", b"again")?;
        tables.native.sync_all().map_err(fs_error)?;
        check!(
            root_state(rig) & 1 == 0,
            "stop {stop}: a clean stop laundered an unchecked root"
        );
    }
    check_root(rig);
    for stop in 0..3 {
        let mut tables = boot(rig)?;
        check!(
            root_state(rig) == 1,
            "stop {stop}: a checked root booted dirty"
        );
        put(&mut tables.abi, "/f", b"clean")?;
        tables.native.sync_all().map_err(fs_error)?;
        check!(root_state(rig) == 1, "stop {stop}: the sync left it dirty");
    }
    Ok(())
}

/// One boot of the soak: check what the last one left, write `/gen` through
/// the table `roll` picks, then stop cleanly or (with a check after) not.
fn generation(rig: &Rig, n: u32, expected: &mut Vec<u8>, roll: u32) -> Result<(), String> {
    let mut tables = boot(rig)?;
    check!(root_state(rig) == 1, "generation {n}: booted dirty");
    let id = Id::ROOT;
    if !expected.is_empty() {
        check!(
            tables.native.read_file(id, "/gen").map_err(fs_error)? == *expected,
            "generation {n}: the file changed across a power cycle"
        );
    }
    *expected = (0..1 + roll % 3000).map(|i| (i ^ n) as u8).collect();
    let table = if roll & 1 == 0 {
        &mut tables.native
    } else {
        &mut tables.abi
    };
    put(table, "/gen", expected)?;
    if roll & 2 == 0 {
        tables.native.sync_all().map_err(fs_error)?;
        check!(root_state(rig) == 1, "generation {n}: synced but dirty");
        return Ok(());
    }
    drop(tables); // an unclean stop
    drop(boot(rig)?);
    check!(
        root_state(rig) & 1 == 0,
        "generation {n}: an unclean stop went unseen"
    );
    check_root(rig);
    Ok(())
}

/// Soak: 200 boots of the configured layout, each writing through a table
/// chosen at random and stopping cleanly (native `sync_all`) or not. The next
/// boot's view of the clean bit always matches how the last one stopped, the
/// file survives every hop, and the heap does not grow.
pub fn power_cycle_soak() -> Result<(), String> {
    task::register_kernel();
    let rig = fresh_rig();
    let mut rng = 0x00C0_FFEEu32;
    let mut roll = move || {
        rng ^= rng << 13;
        rng ^= rng >> 17;
        rng ^= rng << 5;
        rng
    };
    let mut expected = Vec::new();
    generation(rig, 0, &mut expected, 0)?; // warm lazily grown allocations
    let measure = || crate::mem::slab::stats().live_bytes + crate::mem::heap_stats().used;
    let before = measure();
    for n in 1..200 {
        generation(rig, n, &mut expected, roll())?;
    }
    let after = measure();
    check!(
        after <= before + 16 * 1024,
        "heap grew from {before} to {after}"
    );
    Ok(())
}
