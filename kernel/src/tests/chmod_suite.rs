//! The native `chmod` syscall (32, `process/fsops.rs`; fs F3, #507): the same
//! owner-or-root rule as the Linux `chmod` (both go through `Vfs::setattr`),
//! permission bits only, `EROFS` on a read-only mount, `ENOENT`/`EFAULT` for a
//! bad path, and a soak that ten thousand calls leave the heap where it was.
//!
//! Each test swaps in its own native mount table: `/` and `/transient` (ramfs,
//! writable) and `/ro` (ramfs mounted `ro`, its file made through a writable
//! table over the same volume first).

use super::*;
use crate::fs::ramfs::RamFs;
use crate::fs::vfs::{AttrRequest, FsError, Id, MountFlags, Vfs};
use crate::ipc::credentials::{self, Cred};
use alloc::sync::Arc;

const SYS_CHMOD: u64 = 32;

const EPERM: i64 = 1;
const ENOENT: i64 = 2;
const EFAULT: i64 = 14;
const EINVAL: i64 = 22;
const EROFS: i64 = 30;

/// Owned by [`USER`], 0644.
const OWN: &str = "/transient/own";
/// Owned by root, 0644.
const ROOTS: &str = "/transient/roots";
/// On the read-only mount, 0644.
const LOCKED: &str = "/ro/file";
const MISSING: &str = "/transient/missing";

/// The owner of [`OWN`].
const USER: Cred = Cred::new(1000, 100, 0, 0, 0);
/// A user who owns nothing here.
const OTHER: Cred = Cred::new(1001, 100, 0, 0, 0);

fn fs_error(error: FsError) -> String {
    String::from(error.message())
}

fn c(text: &str) -> Vec<u8> {
    let mut out = Vec::from(text.as_bytes());
    out.push(0);
    out
}

/// `chmod(path, mode)` through the native gate, as the current task: `0` or
/// `-errno`.
fn chmod(path: &str, mode: u64) -> i64 {
    let path = c(path);
    process::dispatch_for_test(SYS_CHMOD, path.as_ptr() as u64, mode, 0) as i64
}

/// The permission bits of `path` as root sees them.
fn mode_of(path: &str) -> Result<u16, String> {
    crate::fs::vfs_stat(Id::ROOT, path)
        .map(|meta| meta.mode & 0o7777)
        .map_err(fs_error)
}

/// Run `body` as the kernel task over this suite's mount table, then restore
/// the previous table and root credentials.
fn with_table(body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    let (root, transient, locked) = (
        Arc::new(RamFs::new()),
        Arc::new(RamFs::new()),
        Arc::new(RamFs::new()),
    );
    // The read-only volume's file is made through a writable table first.
    let mut setup = Vfs::new();
    setup
        .mount(fhs::mount::ROOT, root.clone(), MountFlags::default())
        .map_err(fs_error)?;
    setup
        .mount("/ro", locked.clone(), MountFlags::default())
        .map_err(fs_error)?;
    setup.set_umask(0);
    setup.create(Id::ROOT, LOCKED, 0o644).map_err(fs_error)?;

    let mut vfs = Vfs::new();
    vfs.mount(fhs::mount::ROOT, root, MountFlags::default())
        .map_err(fs_error)?;
    vfs.mount(fhs::mount::TRANSIENT, transient, MountFlags::default())
        .map_err(fs_error)?;
    let ro = MountFlags {
        ro: true,
        ..MountFlags::default()
    };
    vfs.mount("/ro", locked, ro).map_err(fs_error)?;
    vfs.set_umask(0);
    vfs.create(Id::ROOT, OWN, 0o644).map_err(fs_error)?;
    vfs.setattr(
        Id::ROOT,
        OWN,
        AttrRequest::Owner {
            uid: Some(USER.uid),
            gid: Some(USER.gid),
        },
    )
    .map_err(fs_error)?;
    vfs.create(Id::ROOT, ROOTS, 0o644).map_err(fs_error)?;

    let previous = crate::fs::install_native_for_test(vfs);
    let result = body();
    crate::fs::restore_native_for_test(previous);
    credentials::set(task::KERNEL_TASK, Cred::ROOT);
    task::harness::reset();
    result
}

/// The owner changes its own file, root changes anyone's, and another uid
/// gets `EPERM` with the mode left alone, exactly as Linux `chmod`.
pub fn chmod_owner_or_root() -> Result<(), String> {
    with_table(|| {
        credentials::set(task::KERNEL_TASK, USER);
        let got = chmod(OWN, 0o755);
        check!(got == 0, "the owner's chmod gave {got}");
        check!(mode_of(OWN)? == 0o755, "mode is {:o}", mode_of(OWN)?);

        let got = chmod(ROOTS, 0o777);
        check!(got == -EPERM, "chmod of root's file as uid 1000 gave {got}");
        check!(mode_of(ROOTS)? == 0o644, "a refused chmod changed the mode");

        credentials::set(task::KERNEL_TASK, OTHER);
        let got = chmod(OWN, 0o777);
        check!(got == -EPERM, "chmod by another uid gave {got}");
        check!(mode_of(OWN)? == 0o755, "a refused chmod changed the mode");

        credentials::set(task::KERNEL_TASK, Cred::ROOT);
        for (path, mode) in [(OWN, 0o700), (ROOTS, 0o4755), (ROOTS, 0)] {
            let got = chmod(path, mode);
            check!(got == 0, "root chmod {path} {mode:o} gave {got}");
            check!(
                mode_of(path)? == mode as u16,
                "{path} is {:o}, want {mode:o}",
                mode_of(path)?
            );
        }
        Ok(())
    })
}

/// Any bit above `0o7777` (a type bit, or garbage in the upper register) is
/// `EINVAL`, and the file keeps its mode.
pub fn chmod_rejects_bad_mode_bits() -> Result<(), String> {
    with_table(|| {
        for mode in [0o10_0755, 0o100_644, 0o17_0000, 1 << 16, 1 << 40, u64::MAX] {
            let got = chmod(ROOTS, mode);
            check!(got == -EINVAL, "mode {mode:#x} gave {got}");
            check!(mode_of(ROOTS)? == 0o644, "mode {mode:#x} changed the file");
        }
        Ok(())
    })
}

/// A read-only mount is `EROFS` (even for root), a missing file `ENOENT`, an
/// unmapped path pointer `EFAULT`.
pub fn chmod_erofs_enoent_efault() -> Result<(), String> {
    with_table(|| {
        let got = chmod(LOCKED, 0o755);
        check!(got == -EROFS, "chmod on a read-only mount gave {got}");
        check!(mode_of(LOCKED)? == 0o644, "the read-only file changed");
        let got = chmod(MISSING, 0o755);
        check!(got == -ENOENT, "chmod of a missing file gave {got}");

        let path = c(ROOTS);
        let previous = crate::user_ptr::set_trust_kernel_pointers(false);
        // A kernel-half address is never user memory; null is never mapped.
        let kernel = process::dispatch_for_test(SYS_CHMOD, path.as_ptr() as u64, 0o755, 0) as i64;
        let null = process::dispatch_for_test(SYS_CHMOD, 0, 0o755, 0) as i64;
        crate::user_ptr::set_trust_kernel_pointers(previous);
        check!(kernel == -EFAULT, "a kernel pointer gave {kernel}");
        check!(null == -EFAULT, "a null pointer gave {null}");
        check!(
            mode_of(ROOTS)? == 0o644,
            "a faulting chmod changed the file"
        );
        Ok(())
    })
}

const CYCLES: usize = 10_000;

/// Heap bytes in use (general heap plus slab).
fn heap() -> usize {
    crate::mem::slab::stats().live_bytes as usize + crate::mem::heap_stats().used as usize
}

/// Ten thousand calls, allowed and refused in turn (owner, root, `EPERM`,
/// `EINVAL`, `EROFS`, `ENOENT`), leave the heap where the warm-up left it.
pub fn chmod_soak_no_leak() -> Result<(), String> {
    with_table(|| {
        let calls: [(Cred, &str, u64, i64); 6] = [
            (USER, OWN, 0o755, 0),
            (Cred::ROOT, OWN, 0o644, 0),
            (OTHER, OWN, 0o777, -EPERM),
            (USER, OWN, 0o10_0755, -EINVAL),
            (Cred::ROOT, LOCKED, 0o755, -EROFS),
            (USER, MISSING, 0o755, -ENOENT),
        ];
        let mut cycle = |index: usize| -> Result<(), String> {
            let (cred, path, mode, want) = calls[index % calls.len()];
            credentials::set(task::KERNEL_TASK, cred);
            let got = chmod(path, mode);
            check!(
                got == want,
                "cycle {index}: chmod {path} {mode:o} gave {got}, want {want}"
            );
            Ok(())
        };
        for warm in 0..calls.len() * 2 {
            cycle(warm)?;
        }
        let before = heap();
        for index in 0..CYCLES {
            cycle(index)?;
        }
        let after = heap();
        check!(
            after == before,
            "{CYCLES} chmod calls moved the heap from {before} to {after} bytes"
        );
        Ok(())
    })
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("chmod_owner_or_root", chmod_owner_or_root),
    ("chmod_rejects_bad_mode_bits", chmod_rejects_bad_mode_bits),
    ("chmod_erofs_enoent_efault", chmod_erofs_enoent_efault),
    ("chmod_soak_no_leak", chmod_soak_no_leak),
];
