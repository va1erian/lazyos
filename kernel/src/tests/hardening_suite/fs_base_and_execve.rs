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

/// A header-only ELF64 whose program headers are `PT_LOAD` segments
/// `(vaddr, memsz)`, none backed by file data. It is structurally valid, so
/// `load_segments` gets past the header and maps the earlier segments before
/// tripping over a bad later one: a genuine *partial* load.
fn elf_with_segments(segments: &[(u64, u64)]) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    image.extend_from_slice(&[0; 8]);
    image.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    image.extend_from_slice(&0x3eu16.to_le_bytes()); // x86-64
    image.extend_from_slice(&1u32.to_le_bytes());
    image.extend_from_slice(&0x40_0000u64.to_le_bytes()); // entry
    image.extend_from_slice(&64u64.to_le_bytes()); // phoff
    image.extend_from_slice(&0u64.to_le_bytes()); // shoff
    image.extend_from_slice(&0u32.to_le_bytes()); // flags
    image.extend_from_slice(&64u16.to_le_bytes()); // ehsize
    image.extend_from_slice(&56u16.to_le_bytes()); // phentsize
    image.extend_from_slice(&(segments.len() as u16).to_le_bytes());
    image.extend_from_slice(&[0; 6]); // shentsize, shnum, shstrndx
    for &(vaddr, memsz) in segments {
        image.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        image.extend_from_slice(&6u32.to_le_bytes()); // PF_R | PF_W
        image.extend_from_slice(&0u64.to_le_bytes()); // offset
        image.extend_from_slice(&vaddr.to_le_bytes());
        image.extend_from_slice(&vaddr.to_le_bytes());
        image.extend_from_slice(&0u64.to_le_bytes()); // filesz
        image.extend_from_slice(&memsz.to_le_bytes());
        image.extend_from_slice(&0x1000u64.to_le_bytes());
    }
    image
}

/// Segments the loader must refuse after mapping an acceptable first one: one
/// at/above the 512 GiB that `free_user_table` reclaims, and one whose end
/// wraps around the address space.
pub fn out_of_range_segment_is_refused_without_leaking() -> Result<(), String> {
    fresh()?;
    for (what, bad, reason) in [
        ("above 512 GiB", (1u64 << 39, 0x1000u64), "loadable range"),
        ("wrapping end", (0x80_0000, u64::MAX), "wraps"),
    ] {
        let elf = elf_with_segments(&[(0x40_0000, 0x3000), bad]);
        let before = mem::frame_stats().live();
        let table = mem::new_user_table().ok_or("out of memory")?;
        let guard = mem::UserTableGuard::new(table);
        let result = process::load_segments(guard.table(), &elf, &[]);
        // Match the reason too, so an unrelated rejection can't pass for it.
        check!(
            matches!(result, Err(message) if message.contains(reason)),
            "a segment {what} -> {result:?}, expected a refusal mentioning {reason:?}"
        );
        drop(guard);
        let after = mem::frame_stats().live();
        check!(
            after == before,
            "a refused segment {what} leaked {} frames",
            after as i64 - before as i64
        );
    }
    Ok(())
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

    // A partial load (first segment mapped, second refused) is the case that
    // actually strands frames beyond the PML4 itself.
    let partial = elf_with_segments(&[(0x40_0000, 0x3000), (1u64 << 39, 0x1000)]);
    let partial_name = "/tmp/lazyos-execve-partial";
    crate::fs::abi_create(Id::current(), partial_name, 0o755).map_err(|e| e.message())?;
    crate::fs::abi_write(Id::current(), partial_name, 0, &partial).map_err(|e| e.message())?;
    let partial_path = b"/tmp/lazyos-execve-partial\0";
    let code = process::linux::dispatch_for_test(SYS_EXECVE, partial_path.as_ptr() as u64, 0, 0);
    check!(
        code == failed(ENOEXEC),
        "partial-load execve -> {code:#x}, expected -ENOEXEC"
    );

    let before = mem::frame_stats().live();
    let code = process::linux::dispatch_for_test(SYS_EXECVE, partial_path.as_ptr() as u64, 0, 0);
    check!(
        code == failed(ENOEXEC),
        "partial-load execve -> {code:#x}, expected -ENOEXEC"
    );
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
