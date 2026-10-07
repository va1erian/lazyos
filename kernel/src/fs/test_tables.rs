//! Test hooks that swap the native and Linux ABI mount tables (split out of
//! `fs/mod.rs`); compiled only with `LAZYOS_TESTS=1`.

use alloc::sync::Arc;

use super::vfs::{Filesystem, MountFlags, Vfs};
use super::{mounts, ramfs, ABI_FS, FS};

/// Install a fresh Linux ABI mount table backed entirely by ramfs (issue
/// #229's leak test): the test suite boots without [`init`] having mounted a
/// boot volume, so the ABI table would otherwise be `None` and no path could
/// reach `execve`'s load path.
pub fn install_abi_ramfs_for_test() {
    let mut abi = Vfs::new();
    let _ = abi.mount(
        fhs::mount::ROOT,
        Arc::new(ramfs::RamFs::new()),
        MountFlags::default(),
    );
    let _ = abi.mount(
        fhs::mount::TMP,
        Arc::new(ramfs::RamFs::new()),
        MountFlags::default(),
    );
    *ABI_FS.lock() = Some(abi);
}

/// Swap in a Linux ABI table whose `/data` is `volume` (over a ramfs root and
/// `/tmp`), returning the table it replaced so a test can put it back with
/// [`restore_abi_for_test`]. This is what a boot with a data disk builds,
/// without needing a second block device.
pub fn install_abi_data_for_test(volume: Arc<dyn Filesystem>) -> Option<Vfs> {
    let mut abi = Vfs::new();
    let _ = abi.mount(
        fhs::mount::ROOT,
        Arc::new(ramfs::RamFs::new()),
        MountFlags::default(),
    );
    let _ = abi.mount(
        fhs::mount::TMP,
        Arc::new(ramfs::RamFs::new()),
        MountFlags::default(),
    );
    let _ = abi.mount(mounts::DATA_MOUNT, volume, MountFlags::default());
    ABI_FS.lock().replace(abi)
}

/// Swap in `table` as the Linux ABI mount table, returning the one it replaced.
pub fn install_abi_for_test(table: Vfs) -> Option<Vfs> {
    ABI_FS.lock().replace(table)
}

/// Swap in `table` as the native mount table (reported as mounted), returning
/// the one it replaced so a test can put it back with
/// [`restore_native_for_test`].
pub fn install_native_for_test(table: Vfs) -> Option<(Vfs, bool)> {
    FS.lock().replace((table, true))
}

/// Put back the table [`install_native_for_test`] returned.
pub fn restore_native_for_test(previous: Option<(Vfs, bool)>) {
    *FS.lock() = previous;
}

/// Put back the table [`install_abi_data_for_test`] returned.
pub fn restore_abi_for_test(previous: Option<Vfs>) {
    *ABI_FS.lock() = previous;
}
