//! The supervisor's kernel surface: `task::spawn_child` sets the
//! parent link (so a service is reapable) and the native `wait`
//! syscall packs the exit status into one register. `init` builds its
//! restart loop on exactly this. Service supervision primitives
//! (issue #93).

use super::*;

/// A minimal but valid static ELF64: one `PT_LOAD` segment with a single
/// `hlt` byte. The loader maps it; the test never runs it.
pub fn minimal_elf() -> Vec<u8> {
    let mut elf = vec![0u8; 0x120];
    elf[0..4].copy_from_slice(b"\x7fELF");
    elf[4] = 2; // ELFCLASS64
    elf[5] = 1; // little-endian
    elf[6] = 1; // version
    elf[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    elf[18..20].copy_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    elf[20..24].copy_from_slice(&1u32.to_le_bytes()); // version
    elf[24..32].copy_from_slice(&0x40_0000u64.to_le_bytes()); // entry
    elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
    elf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
    elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
    elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
    let ph = 64;
    elf[ph..ph + 4].copy_from_slice(&1u32.to_le_bytes()); // PT_LOAD
    elf[ph + 4..ph + 8].copy_from_slice(&5u32.to_le_bytes()); // R|X
    elf[ph + 8..ph + 16].copy_from_slice(&0x100u64.to_le_bytes()); // p_offset
    elf[ph + 16..ph + 24].copy_from_slice(&0x40_0000u64.to_le_bytes()); // p_vaddr
    elf[ph + 24..ph + 32].copy_from_slice(&0x40_0000u64.to_le_bytes()); // p_paddr
    elf[ph + 32..ph + 40].copy_from_slice(&1u64.to_le_bytes()); // p_filesz
    elf[ph + 40..ph + 48].copy_from_slice(&1u64.to_le_bytes()); // p_memsz
    elf[ph + 48..ph + 56].copy_from_slice(&0x1000u64.to_le_bytes()); // p_align
    elf[0x100] = 0xf4; // hlt
    elf
}

fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
}

/// `spawn_child` links the new task to the caller; after it exits, the
/// native `wait` syscall reaps it and returns `(pid << 32) | status`.
pub fn spawn_child_parent_and_wait() -> Result<(), String> {
    fresh();
    let elf = minimal_elf();
    let slot = task::spawn_child("svc", &elf).map_err(to_string)?;
    check!(
        (1..task::MAX_TASKS).contains(&slot),
        "spawn_child returned slot {slot}"
    );
    check!(
        task::process::ppid_of(slot) == task::KERNEL_TASK,
        "child ppid is {} (expected the kernel slot)",
        task::process::ppid_of(slot)
    );
    check!(task::reap_child().is_none(), "a running child was reapable");
    task::harness::finish(slot, 7);
    let packed = process::dispatch_for_test(7, 0, 0, 0);
    check!(
        packed != u64::MAX,
        "wait reported a timeout for a dead child"
    );
    check!(
        packed >> 32 == slot as u64,
        "wait returned pid {} (expected {slot})",
        packed >> 32
    );
    check!(
        packed & 0xffff_ffff == 7,
        "wait returned status {} (expected 7)",
        packed & 0xffff_ffff
    );
    check!(task::reap_child().is_none(), "the child was not reaped");
    Ok(())
}

/// A malformed image is refused without leaving a task slot or frames
/// behind, so a supervisor retry cannot leak.
pub fn spawn_child_rejects_bad_image() -> Result<(), String> {
    fresh();
    let result = task::spawn_child("bad", b"not an ELF image");
    check!(result.is_err(), "a malformed ELF was accepted");
    check!(
        task::snapshot(1).is_none(),
        "the failed spawn left a task in slot 1"
    );
    Ok(())
}

/// The native `spawn` syscall refuses a missing FAT entry with `u64::MAX`
/// (the test harness boots before `fs::init`, so every name is missing).
pub fn spawn_unknown_file_fails() -> Result<(), String> {
    fresh();
    static MISSING: &[u8] = b"NOSUCH.ELF\0";
    let packed = process::dispatch_for_test(6, MISSING.as_ptr() as u64, 0, 0);
    check!(
        packed == u64::MAX,
        "spawn of a missing file returned {packed:#x}"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "service_spawn_child_parent_and_wait",
        spawn_child_parent_and_wait,
    ),
    (
        "service_spawn_child_rejects_bad_image",
        spawn_child_rejects_bad_image,
    ),
    ("service_spawn_unknown_file_fails", spawn_unknown_file_fails),
];
