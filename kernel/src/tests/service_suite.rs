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

/// The `linux:` prefix selects the Linux ABI; everything else is the native
/// default. Malformed lines name no program.
pub fn spawn_line_parses_personality() -> Result<(), String> {
    use crate::process::spawn_line::{parse, SpawnLine};
    let native = parse("  TOP.ELF a b  ");
    check!(
        native
            == Some(SpawnLine {
                linux: false,
                path: "TOP.ELF",
                args: "a b"
            }),
        "native line parsed as {native:?}"
    );
    let linux = parse("linux:XTERM.ELF --client attempt=1");
    check!(
        linux
            == Some(SpawnLine {
                linux: true,
                path: "XTERM.ELF",
                args: "--client attempt=1"
            }),
        "linux line parsed as {linux:?}"
    );
    let spaced = parse("linux:  XSYSMON.ELF");
    check!(
        spaced
            == Some(SpawnLine {
                linux: true,
                path: "XSYSMON.ELF",
                args: ""
            }),
        "linux line with spacing parsed as {spaced:?}"
    );
    for empty in ["", "   ", "linux:", "linux:   "] {
        check!(parse(empty).is_none(), "{empty:?} parsed as a program");
    }
    Ok(())
}

/// `spawn_linux_child` is a supervised child: it links to the caller, joins
/// its process group and session, and is reapable like a native child.
pub fn spawn_linux_child_is_supervised() -> Result<(), String> {
    fresh();
    let elf = minimal_elf();
    let slot = task::spawn_linux_child("app", &elf, &["app", "--client"]).map_err(to_string)?;
    check!(
        task::process::ppid_of(slot) == task::KERNEL_TASK,
        "linux child ppid is {}",
        task::process::ppid_of(slot)
    );
    check!(
        task::process::pgid_of(slot) == task::process::pgid_of(task::KERNEL_TASK),
        "linux child did not join the supervisor's group"
    );
    check!(
        task::process::sid_of(slot) == task::process::sid_of(task::KERNEL_TASK),
        "linux child did not join the supervisor's session"
    );
    task::harness::finish(slot, 3);
    let packed = process::dispatch_for_test(7, 0, 0, 0);
    check!(
        packed != u64::MAX && packed >> 32 == slot as u64 && packed & 0xffff_ffff == 3,
        "wait returned {packed:#x} for the finished linux child"
    );
    Ok(())
}

/// A malformed Linux image is refused without leaving a slot behind.
pub fn spawn_linux_child_rejects_bad_image() -> Result<(), String> {
    fresh();
    check!(
        task::spawn_linux_child("bad", b"not an ELF image", &["bad"]).is_err(),
        "a malformed Linux ELF was accepted"
    );
    check!(
        task::snapshot(1).is_none(),
        "the failed spawn left a task in slot 1"
    );
    Ok(())
}

/// The `linux:` spawn path refuses a missing file like the native one.
pub fn spawn_linux_unknown_file_fails() -> Result<(), String> {
    fresh();
    static MISSING: &[u8] = b"linux:NOSUCH.ELF --client\0";
    let packed = process::dispatch_for_test(6, MISSING.as_ptr() as u64, 0, 0);
    check!(
        packed == u64::MAX,
        "linux spawn of a missing file returned {packed:#x}"
    );
    Ok(())
}

/// The Linux spawn loader resolves a bare applet name (`sh`, `/bin/ls`) to the
/// `BUSYBOX` file — how `logind` starts the user's shell — while a real path
/// still reads its own bytes and a non-applet miss stays missing (issue #254).
pub fn linux_load_executable_resolves_applets() -> Result<(), String> {
    fresh();
    crate::fs::install_abi_ramfs_for_test();
    // Install as root (the image builder owns the boot volume); the lookup
    // under test reads with the running identity.
    let id = crate::fs::vfs::Id::ROOT;
    let elf = minimal_elf();
    crate::fs::abi_create(id, fhs::boot::BUSYBOX_PATH, 0o755).map_err(|e| e.message())?;
    crate::fs::abi_write(id, fhs::boot::BUSYBOX_PATH, 0, &elf).map_err(|e| e.message())?;
    crate::fs::abi_create(id, "/ref", 0o644).map_err(|e| e.message())?;
    crate::fs::abi_write(id, "/ref", 0, b"ref-bytes").map_err(|e| e.message())?;

    check!(
        process::linux::load_executable("sh") == Some(elf.clone()),
        "`sh` did not resolve to the BUSYBOX applet alias"
    );
    check!(
        process::linux::load_executable("/bin/ls") == Some(elf.clone()),
        "/bin/ls did not resolve to the BUSYBOX applet alias"
    );
    check!(
        process::linux::load_executable("/ref") == Some(b"ref-bytes".to_vec()),
        "a real file did not resolve to its own bytes"
    );
    check!(
        process::linux::load_executable("no.such").is_none(),
        "a dotted non-applet name resolved to something"
    );
    Ok(())
}

/// Install BusyBox and the given `(path, bytes)` files on a fresh ABI ramfs.
fn install_exec_files(files: &[(&str, &[u8])]) -> Result<(), String> {
    crate::fs::install_abi_ramfs_for_test();
    let id = crate::fs::vfs::Id::ROOT;
    for (path, bytes) in files {
        crate::fs::abi_create(id, path, 0o755).map_err(|e| e.message())?;
        crate::fs::abi_write(id, path, 0, bytes).map_err(|e| e.message())?;
    }
    Ok(())
}

/// A program shipped at the image root (`/RHAI.ELF`, issue #319) is what
/// `rhai`, `/bin/rhai` and `/usr/local/bin/rhai` load — ahead of the BusyBox
/// alias that claims every plain `bin` name — while data files without the
/// `.ELF` suffix never shadow an applet and a real file at the path wins.
pub fn linux_load_executable_prefers_root_elf() -> Result<(), String> {
    fresh();
    let busybox = minimal_elf();
    install_exec_files(&[
        (fhs::boot::BUSYBOX_PATH, &busybox),
        ("/RHAI.ELF", b"rhai-program"),
        ("/PASSWD", b"root:0:0"),
        ("/rhai2", b"exact-file"),
        ("/TOOLONGNAME.ELF", b"too-long"),
    ])?;
    // `/usr/local/sbin` heads BusyBox ash's default `$PATH`; an unbacked
    // directory such as `/opt/x/bin` falls back to the basename, which must
    // still find the image-root program before the BusyBox alias.
    for name in [
        "rhai",
        "/bin/rhai",
        "/usr/local/bin/rhai",
        "/usr/local/sbin/rhai",
        "/usr/bin/RHAI",
        "/opt/x/bin/rhai",
    ] {
        check!(
            process::linux::load_executable(name) == Some(b"rhai-program".to_vec()),
            "`{name}` did not resolve to /RHAI.ELF"
        );
    }
    check!(
        process::linux::load_executable("/RHAI.ELF") == Some(b"rhai-program".to_vec()),
        "the exact image-root path did not load"
    );
    // The applet of the same name as a data file is untouched.
    check!(
        process::linux::load_executable("passwd") == Some(busybox.clone()),
        "PASSWD shadowed the `passwd` applet"
    );
    check!(
        process::linux::load_executable("ls") == Some(busybox.clone()),
        "`ls` did not fall back to BusyBox"
    );
    check!(
        process::linux::load_executable("/rhai2") == Some(b"exact-file".to_vec()),
        "a real file at the exact path lost to the alias"
    );
    // Names longer than 8.3 cannot be FAT root files, so they never probe.
    check!(
        process::linux::load_executable("toolongname") == Some(busybox.clone()),
        "a name longer than 8 characters was resolved at the image root"
    );
    // Misses: no ELF and no BusyBox means nothing to run.
    install_exec_files(&[("/PASSWD", b"root:0:0")])?;
    check!(
        process::linux::load_executable("rhai").is_none(),
        "`rhai` resolved with neither /RHAI.ELF nor BusyBox present"
    );
    check!(
        process::linux::load_executable("/bin/passwd").is_none(),
        "a data file was executed as an applet"
    );
    Ok(())
}

/// Soak: repeated resolution of root programs, applets and misses stays
/// correct and leaks nothing (every `execvp` walks the whole `$PATH`).
pub fn soak_linux_load_executable_repeated() -> Result<(), String> {
    fresh();
    let busybox = minimal_elf();
    install_exec_files(&[
        (fhs::boot::BUSYBOX_PATH, &busybox),
        ("/RHAI.ELF", b"rhai-program"),
    ])?;
    let path_dirs = [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/bin",
        "/usr/bin",
        "/sbin",
    ];
    // Warm-up absorbs one-time allocations so the steady state is compared.
    let _ = process::linux::load_executable("rhai");
    let frames_before = mem::frame_stats().live();
    for round in 0..2000u32 {
        let dir = path_dirs[round as usize % path_dirs.len()];
        let rhai = alloc::format!("{dir}/rhai");
        check!(
            process::linux::load_executable(&rhai) == Some(b"rhai-program".to_vec()),
            "round {round}: {rhai} did not resolve to /RHAI.ELF"
        );
        let ls = alloc::format!("{dir}/ls");
        check!(
            process::linux::load_executable(&ls) == Some(busybox.clone()),
            "round {round}: {ls} did not resolve to BusyBox"
        );
        check!(
            process::linux::load_executable("no.such").is_none(),
            "round {round}: a dotted miss resolved"
        );
    }
    let frames_after = mem::frame_stats().live();
    check!(
        frames_after <= frames_before,
        "live frames grew from {frames_before} to {frames_after} over 2000 rounds"
    );
    Ok(())
}

/// Soak: many supervised Linux children spawn, exit and are reaped without
/// leaking frames or task slots (a desktop session restarts its apps).
pub fn soak_spawn_linux_child_generations() -> Result<(), String> {
    fresh();
    let elf = minimal_elf();
    // One warm-up generation absorbs one-time allocations (tables, interned
    // names) so the steady state is what is compared.
    let warm = task::spawn_linux_child("soak", &elf, &["soak"]).map_err(to_string)?;
    task::harness::finish(warm, 0);
    let _ = process::dispatch_for_test(7, 0, 0, 0);
    let frames_before = mem::frame_stats().live();
    for generation in 0..256u64 {
        let slot = task::spawn_linux_child("soak", &elf, &["soak", "--client"])
            .map_err(|error| format!("generation {generation}: {error}"))?;
        task::harness::finish(slot, generation & 0xff);
        let packed = process::dispatch_for_test(7, 0, 0, 0);
        check!(
            packed != u64::MAX && packed >> 32 == slot as u64,
            "generation {generation}: wait returned {packed:#x}"
        );
    }
    let frames_after = mem::frame_stats().live();
    check!(
        frames_after <= frames_before,
        "live frames grew from {frames_before} to {frames_after} over 256 generations"
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
    (
        "service_spawn_line_parses_personality",
        spawn_line_parses_personality,
    ),
    (
        "service_spawn_linux_child_is_supervised",
        spawn_linux_child_is_supervised,
    ),
    (
        "service_spawn_linux_child_rejects_bad_image",
        spawn_linux_child_rejects_bad_image,
    ),
    (
        "service_spawn_linux_unknown_file_fails",
        spawn_linux_unknown_file_fails,
    ),
    (
        "service_linux_load_executable_resolves_applets",
        linux_load_executable_resolves_applets,
    ),
    (
        "service_linux_load_executable_prefers_root_elf",
        linux_load_executable_prefers_root_elf,
    ),
    (
        "service_soak_linux_load_executable_repeated",
        soak_linux_load_executable_repeated,
    ),
    (
        "service_soak_spawn_linux_child_generations",
        soak_spawn_linux_child_generations,
    ),
];
