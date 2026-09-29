//! Two Linux-ABI hardening regressions: a user-supplied `%fs` base that is
//! non-canonical must be refused before it reaches `wrmsr` (issue #222), and a
//! failed `execve` must release the address space it built (issue #229).

use super::*;

/// `arch_prctl(ARCH_SET_FS)` (0x1002) / `clone` (56) syscall numbers.
const SYS_CLONE: u64 = 56;
const SYS_ARCH_PRCTL: u64 = 158;
const ARCH_SET_FS: u64 = 0x1002;

/// `clone` flags: `CLONE_VM | CLONE_THREAD | CLONE_SETTLS`.
const THREAD_WITH_TLS: u64 = 0x100 | 0x0001_0000 | 0x0008_0000;

/// `execve` (59).
const SYS_EXECVE: u64 = 59;

const ENOEXEC: i64 = 8;
const EINVAL: i64 = 22;

/// A `%fs` base is user supplied; a non-canonical one written with `wrmsr`
/// raises #GP in ring 0 (issue #222). Both entry points must refuse it:
/// `arch_prctl(ARCH_SET_FS)` normally, and `clone(CLONE_SETTLS)` before the
/// value is stored in the new task, so the context-switch MSR write stays
/// infallible by construction. A canonical value still works.
pub fn noncanonical_fs_base_is_rejected() -> Result<(), String> {
    fresh()?;

    for bad in [0x0000_8000_0000_0000u64, 0x8000_0000_0000_0001, u64::MAX] {
        let code = process::linux::dispatch_for_test(SYS_ARCH_PRCTL, ARCH_SET_FS, bad, 0);
        check!(
            code == failed(EINVAL),
            "arch_prctl SET_FS {bad:#x} -> {code:#x}, expected -EINVAL"
        );
    }
    let code =
        process::linux::dispatch_for_test(SYS_ARCH_PRCTL, ARCH_SET_FS, 0x0000_7fff_ffff_f000, 0);
    check!(code == 0, "arch_prctl SET_FS canonical -> {code:#x}");

    // A bad TLS is the caller's error, not an out-of-memory condition, and it
    // must be rejected before a task slot is spent.
    let free_before = task::free_slots();
    for bad in [0x0000_8000_0000_0000u64, 0xffff_ffff_ffff_ffff] {
        let code =
            process::linux::dispatch_args5_for_test(SYS_CLONE, THREAD_WITH_TLS, 0, 0, 0, bad);
        check!(
            code == failed(EINVAL),
            "clone SETTLS {bad:#x} -> {code:#x}, expected -EINVAL"
        );
    }
    check!(
        task::free_slots() == free_before,
        "a rejected clone changed the slot count: {} -> {}",
        free_before,
        task::free_slots()
    );
    Ok(())
}

/// A file that is not a valid ELF: `sys_execve` builds the new address space
/// (`new_user_table`) and only then discovers the image cannot load. Before the
/// fix that error path abandoned the table, leaking its PML4 frame on every
/// attempt; the guard in `sys_execve` (and the spawn paths) now reclaims it.
fn corrupt_elf() -> Vec<u8> {
    b"this is not an ELF image\n".to_vec()
}

/// A loop of failing `execve`s must not leak frames (issue #229): before the
/// fix every attempt abandoned the freshly built PML4 (and every frame a
/// partial load had mapped), so a process could exhaust physical memory for the
/// whole system. The guard in `sys_execve` (and the spawn paths) frees the
/// address space on every failure.
pub fn execve_failure_releases_address_space() -> Result<(), String> {
    fresh()?;
    crate::fs::install_abi_ramfs_for_test();
    let name = "/tmp/lazyos-execve-leak";
    let elf = corrupt_elf();
    crate::fs::abi_create(Id::current(), name, 0o755).map_err(|e| e.message())?;
    crate::fs::abi_write(Id::current(), name, 0, &elf).map_err(|e| e.message())?;

    let path = b"/tmp/lazyos-execve-leak\0";
    // Warm-up: the fixture must reach the load path and fail there (not be
    // refused as a missing/executable-less file), or it would not exercise the
    // address-space leak at all.
    let code = process::linux::dispatch_for_test(SYS_EXECVE, path.as_ptr() as u64, 0, 0);
    check!(
        code == failed(ENOEXEC),
        "corrupt execve -> {code:#x}, expected -ENOEXEC"
    );

    let before = mem::frame_stats().live();
    for attempt in 0..8 {
        let code = process::linux::dispatch_for_test(SYS_EXECVE, path.as_ptr() as u64, 0, 0);
        check!(
            code == failed(ENOEXEC),
            "attempt {attempt}: corrupt execve -> {code:#x}"
        );
    }
    let after = mem::frame_stats().live();
    check!(
        after == before,
        "8 failing execve calls leaked {} frames ({before} -> {after})",
        after as i64 - before as i64
    );
    Ok(())
}
