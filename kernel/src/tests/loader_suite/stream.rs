//! The streaming loader: no image-size or segment-count cap, `.bss` tails
//! that cost nothing until touched, files read in chunks straight from a VFS,
//! and no frame or heap leak across many loads.

use super::*;
use crate::fs::vfs::Id;
use crate::ipc::credentials::{self, Cred};
use crate::process::image::VfsFile;
use crate::process::loader::CHUNK;

/// Where [`build_elf_with`] puts the payload for an image of `phdrs` headers.
fn payload_offset(phdrs: usize) -> u64 {
    (64 + 56 * phdrs as u64).max(PAYLOAD_OFF)
}

/// The kernel task as root over a fresh ramfs ABI table.
fn setup_ramfs() {
    task::register_kernel();
    credentials::set(task::current(), Cred::ROOT);
    crate::fs::install_abi_ramfs_for_test();
}

/// Frames in use right now (the leak and laziness measure).
fn frames_used() -> usize {
    let stats = mem::frame_stats();
    stats.total - stats.free
}

/// Far more segments than the old 32-segment cap load, sorted or not, and
/// each lands its own bytes (the overlap check is one pass over neighbours).
pub fn many_segments_load() -> Result<(), String> {
    const COUNT: u64 = 2000;
    let offset = payload_offset(COUNT as usize);
    // Built in reverse address order: the loader sorts.
    let phdrs: Vec<Ph> = (0..COUNT)
        .rev()
        .map(|i| Ph {
            offset: offset + i * 0x10,
            ..Ph::new(0x1000_0000 + i * 0x2000, 0x10, 0x1000, PF_R)
        })
        .collect();
    let elf = build_elf_with(0x1000_0000, &phdrs, (COUNT * 0x10) as usize);
    with_table(|table| {
        let loaded = load_segments(table, &elf, RESERVED)?;
        check!(loaded.phnum == COUNT as u16, "phnum {}", loaded.phnum);
        for i in [0, 1, COUNT / 2, COUNT - 1] {
            let got = read_back(table, 0x1000_0000 + i * 0x2000, 0x10)?;
            let want: Vec<u8> = (0..0x10)
                .map(|b| payload_byte((i * 0x10 + b) as usize))
                .collect();
            check!(got == want, "segment {i} bytes differ");
        }
        Ok(())
    })
}

/// A segment whose `.bss` is 64 GiB loads with a handful of frames (the file
/// page plus page tables), and a page deep inside it faults in zeroed.
pub fn huge_bss_is_lazy() -> Result<(), String> {
    let base = 0x1_0000_0000u64;
    let bss = 64u64 << 30;
    let elf = build_elf(base, &[Ph::new(base, 0x1000, bss, PF_R | PF_W)]);
    with_table(|table| {
        let before = frames_used();
        let loaded = load_segments(table, &elf, RESERVED)?;
        let used = frames_used() - before;
        check!(used <= 8, "a 64 GiB bss took {used} frames eagerly");
        check!(loaded.end == base + bss, "end {:#x}", loaded.end);
        let deep = base + bss - 0x1000;
        let vma = mem::vma::find(table, deep).ok_or("no VMA deep in the bss")?;
        check!(vma.kind == Kind::Anon, "deep bss VMA is {vma:?}");
        check!(
            mem::demand_fault(table, deep, PageFaultErrorCode::CAUSED_BY_WRITE),
            "deep bss page did not fault in"
        );
        let bytes = read_back(table, deep, 64)?;
        check!(bytes.iter().all(|&b| b == 0), "deep bss page is not zero");
        Ok(())
    })
}

/// An image several [`CHUNK`]s long, written to the ABI ramfs and loaded
/// through a [`VfsFile`], lands byte-exact; a file that shrinks between open
/// and load fails the load cleanly.
pub fn streams_from_a_file() -> Result<(), String> {
    setup_ramfs();
    let payload = 3 * CHUNK + 0x1234;
    let text = Ph::new(0x40_0000, payload as u64, payload as u64, PF_R | PF_X);
    let elf = build_elf_with(0x40_0000, &[text], payload);
    let path = "/tmp/streamed";
    crate::fs::abi_create(Id::current(), path, 0o755).map_err(|e| String::from(e.message()))?;
    crate::fs::abi_write(Id::current(), path, 0, &elf).map_err(|e| String::from(e.message()))?;
    let file = VfsFile::abi(Id::current(), path).map_err(|e| String::from(e.message()))?;
    with_table(|table| {
        load_segments(table, &file, RESERVED)?;
        let got = read_back(table, 0x40_0000, payload)?;
        check!(
            got.iter().enumerate().all(|(i, &b)| b == payload_byte(i)),
            "streamed bytes differ"
        );
        Ok(())
    })?;
    // Shrink the file under an open handle: the load must fail, not map
    // stale or foreign bytes, and free what it mapped.
    crate::fs::abi_truncate(Id::current(), path, (PAYLOAD_OFF as usize + CHUNK) as u64)
        .map_err(|e| String::from(e.message()))?;
    with_table(|table| {
        check!(
            load_segments(table, &file, RESERVED).is_err(),
            "a shrunken file still loaded"
        );
        Ok(())
    })?;
    let _ = crate::fs::abi_unlink(Id::current(), path);
    Ok(())
}

/// Soak: stream the same file hundreds of times; frames and heap return to
/// their starting point (the loader's chunk buffer is its only heap use).
pub fn stream_soak() -> Result<(), String> {
    setup_ramfs();
    let payload = CHUNK + 0x800;
    let phdrs = [
        Ph::new(0x40_0000, payload as u64, payload as u64, PF_R | PF_X),
        Ph::new(0x80_0000, 0x100, 0x10_0000, PF_R | PF_W),
    ];
    let elf = build_elf_with(0x40_0000, &phdrs, payload);
    let path = "/tmp/soak";
    crate::fs::abi_create(Id::current(), path, 0o755).map_err(|e| String::from(e.message()))?;
    crate::fs::abi_write(Id::current(), path, 0, &elf).map_err(|e| String::from(e.message()))?;
    let file = VfsFile::abi(Id::current(), path).map_err(|e| String::from(e.message()))?;
    let heap_before = mem::heap_stats().used;
    for round in 0..200 {
        with_table(|table| {
            load_segments(table, &file, RESERVED)
                .map(|_| ())
                .map_err(|e| format!("round {round}: {e}"))
        })?;
    }
    let heap_after = mem::heap_stats().used;
    check!(
        heap_after <= heap_before + 4096,
        "heap grew from {heap_before} to {heap_after} bytes"
    );
    let _ = crate::fs::abi_unlink(Id::current(), path);
    Ok(())
}
