//! ELF loader hardening (issue #228): every `PT_LOAD` header is untrusted, so
//! malformed images must be refused before anything is mapped, valid ones must
//! land byte-exact, and repeated loads must return every frame.

use super::*;
use crate::process::load_segments;

/// Bytes of program headers/padding before the payload in a test image.
const PAYLOAD_OFF: u64 = 0x1000;
/// Payload length appended to every test image.
const PAYLOAD_LEN: usize = 0x3000;

const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

/// One program header of a synthetic image.
#[derive(Clone, Copy)]
struct Ph {
    vaddr: u64,
    memsz: u64,
    filesz: u64,
    offset: u64,
    flags: u32,
}

impl Ph {
    /// A segment whose file bytes come from the start of the payload.
    fn new(vaddr: u64, filesz: u64, memsz: u64, flags: u32) -> Ph {
        Ph {
            vaddr,
            memsz,
            filesz,
            offset: PAYLOAD_OFF,
            flags,
        }
    }
}

/// Byte `i` of the payload: never zero, so a missed copy cannot pass.
fn payload_byte(i: usize) -> u8 {
    (i as u8).wrapping_mul(7) | 1
}

/// Assemble a minimal ELF64 executable with `phdrs` and a fixed payload.
fn build_elf(entry: u64, phdrs: &[Ph]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&[0x7f, b'E', b'L', b'F', 2, 1, 1, 0]);
    out.extend_from_slice(&[0; 8]);
    out.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
    out.extend_from_slice(&0x3eu16.to_le_bytes()); // x86-64
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&entry.to_le_bytes());
    out.extend_from_slice(&64u64.to_le_bytes()); // phoff
    out.extend_from_slice(&0u64.to_le_bytes()); // shoff
    out.extend_from_slice(&0u32.to_le_bytes()); // flags
    out.extend_from_slice(&64u16.to_le_bytes()); // ehsize
    out.extend_from_slice(&56u16.to_le_bytes()); // phentsize
    out.extend_from_slice(&(phdrs.len() as u16).to_le_bytes());
    out.extend_from_slice(&[0; 6]); // shentsize, shnum, shstrndx
    for ph in phdrs {
        out.extend_from_slice(&1u32.to_le_bytes()); // PT_LOAD
        out.extend_from_slice(&ph.flags.to_le_bytes());
        out.extend_from_slice(&ph.offset.to_le_bytes());
        out.extend_from_slice(&ph.vaddr.to_le_bytes());
        out.extend_from_slice(&ph.vaddr.to_le_bytes()); // paddr
        out.extend_from_slice(&ph.filesz.to_le_bytes());
        out.extend_from_slice(&ph.memsz.to_le_bytes());
        out.extend_from_slice(&0x1000u64.to_le_bytes());
    }
    out.resize(PAYLOAD_OFF as usize, 0);
    out.extend((0..PAYLOAD_LEN).map(payload_byte));
    out
}

/// Run `f` against a fresh user table and free it, asserting the free-frame
/// count returns to where it started (no leaked frame on any path).
fn with_table<R>(f: impl FnOnce(PhysAddr) -> Result<R, String>) -> Result<R, String> {
    let before = mem::frame_stats().free;
    let table = mem::new_user_table().ok_or("new_user_table failed")?;
    let outcome = f(table);
    mem::free_user_table(table);
    let after = mem::frame_stats().free;
    check!(
        after == before,
        "frames leaked: {before} free before, {after} after"
    );
    outcome
}

/// Load `elf` with the native layout's reserved window and demand rejection.
fn expect_rejected(name: &str, elf: &[u8]) -> Result<(), String> {
    with_table(|table| {
        let before = mem::frame_stats().free;
        let result = load_segments(table, elf, RESERVED);
        check!(result.is_err(), "{name}: malformed image was accepted");
        // Rejection is up front: nothing beyond the bare table may be used.
        let used = before - mem::frame_stats().free;
        check!(
            used == 0,
            "{name}: rejected image still consumed {used} frames"
        );
        Ok(())
    })
}

/// A window standing in for the stack/heap area of the loading process.
const RESERVED: &[(u64, u64)] = &[(0x0100_0000, 0x0200_0000)];

/// Overflowing, kernel-half, out-of-range and degenerate headers are refused.
pub fn loader_rejects_malformed_headers() -> Result<(), String> {
    let text = |v, f, m| Ph::new(v, f, m, PF_R | PF_X);
    let cases: Vec<(&str, u64, Vec<Ph>)> = vec![
        (
            "vaddr+memsz overflow",
            0x40_0000,
            vec![text(u64::MAX - 0x1000, 0x10, 0x4000)],
        ),
        (
            "page-rounding overflow",
            u64::MAX - 8,
            vec![text(u64::MAX - 8, 0, 8)],
        ),
        (
            "kernel-half vaddr",
            0xffff_8000_0000_1000,
            vec![text(0xffff_8000_0000_0000, 0x10, 0x2000)],
        ),
        (
            "straddles user limit",
            0x0000_7fff_ffff_f000,
            vec![text(0x0000_7fff_ffff_f000, 0x10, 0x2000)],
        ),
        (
            "memsz 1 TiB",
            0x40_0000,
            vec![text(0x40_0000, 0x10, 1 << 40)],
        ),
        (
            "one page past the cap",
            0x40_0000,
            vec![text(
                0x40_0000,
                0x10,
                (crate::process::loader::MAX_LOAD_PAGES + 1) * 4096,
            )],
        ),
        (
            "filesz > memsz",
            0x40_0000,
            vec![text(0x40_0000, 0x2000, 0x1000)],
        ),
        (
            "file offset past end",
            0x40_0000,
            vec![Ph {
                offset: 0x100_0000,
                ..text(0x40_0000, 0x10, 0x1000)
            }],
        ),
        (
            "file range overflows u64",
            0x40_0000,
            vec![Ph {
                offset: u64::MAX - 4,
                ..text(0x40_0000, 0x10, 0x1000)
            }],
        ),
        (
            "overlapping segments",
            0x40_0000,
            vec![text(0x40_0000, 0x10, 0x2000), text(0x40_1000, 0x10, 0x1000)],
        ),
        (
            "duplicate segment",
            0x40_0000,
            vec![text(0x40_0000, 0x10, 0x1000), text(0x40_0000, 0x10, 0x1000)],
        ),
        (
            "inside reserved window",
            0x0100_0000,
            vec![text(0x0100_0000, 0x10, 0x1000)],
        ),
        (
            "runs into reserved window",
            0x40_0000,
            vec![text(0x00ff_f000, 0x10, 0x2000)],
        ),
        (
            "entry outside segments",
            0x90_0000,
            vec![text(0x40_0000, 0x10, 0x1000)],
        ),
        ("no loadable segment", 0x40_0000, vec![]),
        (
            "too many segments",
            0x40_0000,
            (0..=crate::process::loader::MAX_LOAD_SEGMENTS as u64)
                .map(|i| text(0x40_0000 + i * 0x2000, 0x10, 0x1000))
                .collect(),
        ),
    ];
    for (name, entry, phdrs) in cases {
        expect_rejected(name, &build_elf(entry, &phdrs))?;
    }
    expect_rejected("truncated file", &build_elf(0x40_0000, &[])[..30])?;
    expect_rejected("garbage", &[0x42; 200])
}

/// Read `len` bytes at `va` out of `table`'s mapped frames.
fn read_back(table: PhysAddr, va: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(len);
    for i in 0..len as u64 {
        let phys = frame_of(table, (va + i) & !0xfff)?;
        let src = mem::phys_to_virt(PhysAddr::new(phys)) + ((va + i) & 0xfff);
        // Safety: the frame is mapped through the physical memory map.
        out.push(unsafe { core::ptr::read_volatile(src.as_ptr::<u8>()) });
    }
    Ok(out)
}

/// A well-formed image lands byte-exact, zero-fills `.bss`, keeps page
/// protections, and two segments may share a boundary page.
pub fn loader_maps_valid_image_exactly() -> Result<(), String> {
    // Text and data share page 0x400000 (data starts mid-page), and the data
    // segment carries a 0x2800-byte bss tail.
    let phdrs = [
        Ph::new(0x40_0000, 0x1800, 0x1800, PF_R | PF_X),
        Ph {
            offset: PAYLOAD_OFF + 0x1800,
            ..Ph::new(0x40_1800, 0x100, 0x2900, PF_R | PF_W)
        },
    ];
    let elf = build_elf(0x40_0010, &phdrs);
    with_table(|table| {
        let entry = load_segments(table, &elf, RESERVED)?;
        check!(entry == 0x40_0010, "entry {entry:#x}");
        let text = read_back(table, 0x40_0000, 0x1800)?;
        check!(
            text.iter().enumerate().all(|(i, &b)| b == payload_byte(i)),
            "text bytes differ"
        );
        let data = read_back(table, 0x40_1800, 0x100)?;
        check!(
            data.iter()
                .enumerate()
                .all(|(i, &b)| b == payload_byte(0x1800 + i)),
            "data bytes differ"
        );
        let bss = read_back(table, 0x40_1900, 0x2800)?;
        check!(bss.iter().all(|&b| b == 0), "bss is not zeroed");
        check!(
            raw_entry(table, 0x40_5000).is_none(),
            "page past the image is mapped"
        );
        let vmas = mem::vma::list(table);
        check!(
            vmas.iter().any(|v| v.start == 0x40_0000),
            "no VMA recorded for the image"
        );
        Ok(())
    })
}

/// A static-PIE image linked at address 0 (musl's default) must still load.
pub fn loader_accepts_image_linked_at_zero() -> Result<(), String> {
    let elf = build_elf(0x1010, &[Ph::new(0, 0x2000, 0x3000, PF_R | PF_X)]);
    with_table(|table| {
        let entry = load_segments(table, &elf, RESERVED)?;
        check!(entry == 0x1010, "entry {entry:#x}");
        let head = read_back(table, 0, 16)?;
        check!(head[0] == payload_byte(0), "page 0 not populated");
        Ok(())
    })
}

/// Soak: load a large image (1 MiB of file data) and a many-segment image
/// hundreds of times. Every load must free its frames, and the per-page copy
/// must stay fast (the old per-byte linear scan took minutes at this size).
pub fn loader_soak_no_leak_and_linear_time() -> Result<(), String> {
    let big = build_elf(
        0x40_0000,
        &[Ph::new(0x40_0000, 0x3000, 0x10_0000, PF_R | PF_W | PF_X)],
    );
    let many: Vec<Ph> = (0..crate::process::loader::MAX_LOAD_SEGMENTS as u64)
        .map(|i| Ph::new(0x40_0000 + i * 0x3000, 0x2000, 0x2800, PF_R | PF_X))
        .collect();
    let many = build_elf(0x40_0000, &many);
    for round in 0..120 {
        for elf in [&big, &many] {
            with_table(|table| {
                load_segments(table, elf, RESERVED)
                    .map(|_| ())
                    .map_err(|e| format!("round {round}: {e}"))
            })?;
        }
    }
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "loader_rejects_malformed_headers",
        loader_rejects_malformed_headers,
    ),
    (
        "loader_maps_valid_image_exactly",
        loader_maps_valid_image_exactly,
    ),
    (
        "loader_accepts_image_linked_at_zero",
        loader_accepts_image_linked_at_zero,
    ),
    (
        "loader_soak_no_leak_and_linear_time",
        loader_soak_no_leak_and_linear_time,
    ),
];
