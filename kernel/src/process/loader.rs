//! The static ELF64 segment loader.
//!
//! An executable is attacker-controlled input: any user who can write a file
//! to `/tmp` and set its exec bit reaches this code through `execve`. So the
//! loader works in two phases. [`plan`] validates every `PT_LOAD` header
//! against the address-space layout without allocating anything; only a plan
//! that passed is handed to [`load_segments`], which maps it. That keeps the
//! arithmetic on untrusted `p_vaddr`/`p_memsz` in one small, testable place
//! and guarantees the eager allocation (done while the caller may hold the
//! task table lock) is bounded by [`MAX_LOAD_PAGES`].

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use x86_64::{PhysAddr, VirtAddr};
use xmas_elf::program::Type as ProgramType;
use xmas_elf::ElfFile;

use crate::mem;
use crate::mem::vma::{Kind, Prot};

/// First address past the user half of the address space; the upper half is
/// shared with the kernel and must never receive a user mapping.
pub const USER_LIMIT: u64 = 0x0000_8000_0000_0000;
/// Most 4 KiB pages one image may map eagerly (32 MiB).
pub const MAX_LOAD_PAGES: u64 = 8192;
/// Most `PT_LOAD` headers accepted; bounds the pairwise overlap check.
pub const MAX_LOAD_SEGMENTS: usize = 32;

const PAGE: u64 = 4096;

/// One validated `PT_LOAD` segment.
struct Segment<'a> {
    vaddr: u64,
    /// Byte end of the in-memory image (`vaddr + p_memsz`).
    vend: u64,
    /// Page-aligned `[start, end)` covering `[vaddr, vaddr + mem_size)`.
    start: u64,
    end: u64,
    prot: Prot,
    /// The file-backed prefix (`p_filesz` bytes); the rest is zero.
    data: &'a [u8],
}

/// A validated image, ready to be mapped.
struct Plan<'a> {
    entry: u64,
    segments: Vec<Segment<'a>>,
}

/// Validate `elf_bytes`' program headers.
///
/// `reserved` lists half-open windows (stack, heap, mmap area...) the image
/// must stay out of, because the kernel maps those itself later.
fn plan<'a>(elf_bytes: &'a [u8], reserved: &[(u64, u64)]) -> Result<Plan<'a>, &'static str> {
    let elf = ElfFile::new(elf_bytes).map_err(|_| "not a valid ELF")?;
    let entry = elf.header.pt2.entry_point();
    let mut segments: Vec<Segment<'a>> = Vec::new();
    let mut total_pages = 0u64;

    for header in elf.program_iter() {
        if header.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let (vaddr, mem_size, file_size) =
            (header.virtual_addr(), header.mem_size(), header.file_size());
        if mem_size == 0 {
            continue;
        }
        if file_size > mem_size {
            return Err("segment filesz exceeds memsz");
        }
        let vend = vaddr.checked_add(mem_size).ok_or("segment wraps")?;
        let end = vend.checked_add(PAGE - 1).ok_or("segment wraps")? & !(PAGE - 1);
        let start = vaddr & !(PAGE - 1);
        // No lower bound: musl's static-PIE fixtures are linked at address 0
        // and relocate themselves, so page 0 is a legal load address here.
        if end > USER_LIMIT {
            return Err("segment outside user address space");
        }
        if reserved.iter().any(|&(lo, hi)| start < hi && lo < end) {
            return Err("segment overlaps reserved region");
        }
        // Distinct segments may share a boundary page but never a byte.
        if segments
            .iter()
            .any(|other| vaddr < other.vend && other.vaddr < vend)
        {
            return Err("overlapping segments");
        }
        if segments.len() >= MAX_LOAD_SEGMENTS {
            return Err("too many segments");
        }
        // Summed per segment, so a page two segments share counts twice: a
        // conservative bound, and it cannot overflow after the checks above.
        total_pages += (end - start) / PAGE;
        if total_pages > MAX_LOAD_PAGES {
            return Err("image too large");
        }

        let offset = header.offset();
        let file_end = offset.checked_add(file_size).ok_or("segment data range")?;
        if file_end > elf_bytes.len() as u64 {
            return Err("segment data out of file");
        }
        let flags = header.flags();
        let mut prot = Prot::READ;
        if flags.is_write() {
            prot = prot | Prot::WRITE;
        }
        if flags.is_execute() {
            prot = prot | Prot::EXEC;
        }
        segments.push(Segment {
            vaddr,
            vend,
            start,
            end,
            prot,
            // In range: `file_end <= len` was just checked.
            data: &elf_bytes[offset as usize..file_end as usize],
        });
    }

    if !segments
        .iter()
        .any(|seg| entry >= seg.vaddr && entry < seg.vend)
    {
        return Err("entry point outside loaded segments");
    }
    Ok(Plan { entry, segments })
}

/// Map a program's `PT_LOAD` segments into `table` and return its entry point.
///
/// Segments are mapped eagerly (their contents must exist before the program
/// runs) with the protection the ELF header asks for: read always, write only
/// for `PF_W`, execute only for `PF_X`. Each segment is recorded as a `File`
/// VMA so `munmap`/`mprotect` and diagnostics see the same layout the hardware
/// does. A page shared by two segments gets the union of their protections
/// (PTE and VMA alike), so neither segment faults on its own accesses.
///
/// On error `table` may hold a partial image; the caller owns it and frees it
/// with [`mem::free_user_table`].
pub fn load_segments(
    table: PhysAddr,
    elf_bytes: &[u8],
    reserved: &[(u64, u64)],
) -> Result<u64, &'static str> {
    let plan = plan(elf_bytes, reserved)?;
    // Page base -> backing frame; a map so per-page lookups stay logarithmic.
    let mut pages: BTreeMap<u64, u64> = BTreeMap::new();
    // Current protection per page, and the pages two segments share.
    let mut prots: BTreeMap<u64, Prot> = BTreeMap::new();
    let mut shared: Vec<u64> = Vec::new();

    for segment in &plan.segments {
        for va in (segment.start..segment.end).step_by(PAGE as usize) {
            if let Some(current) = prots.get_mut(&va) {
                let union = *current | segment.prot;
                if union != *current {
                    if !mem::protect_range(table, va, va + PAGE, union) {
                        return Err("failed to widen shared page");
                    }
                    *current = union;
                }
                shared.push(va);
                continue;
            }
            let phys = mem::alloc_zeroed_frame().ok_or("out of memory")?;
            if !mem::map_page_in(
                table,
                VirtAddr::new(va),
                phys,
                mem::prot_flags(segment.prot),
            ) {
                mem::free_frame(phys);
                return Err("failed to map segment");
            }
            pages.insert(va, phys.as_u64());
            prots.insert(va, segment.prot);
        }
        copy_segment(&pages, segment);
        mem::vma::insert(table, segment.start, segment.end, segment.prot, Kind::File);
    }
    // Segment VMAs were inserted whole; restate the shared pages with the
    // union the PTEs now carry.
    for va in shared {
        mem::vma::insert(table, va, va + PAGE, prots[&va], Kind::File);
    }
    Ok(plan.entry)
}

/// Copy a segment's file bytes into its mapped frames, one page at a time.
fn copy_segment(pages: &BTreeMap<u64, u64>, segment: &Segment<'_>) {
    let mut copied = 0usize;
    while copied < segment.data.len() {
        let va = segment.vaddr + copied as u64;
        let in_page = (va & (PAGE - 1)) as usize;
        let count = (PAGE as usize - in_page).min(segment.data.len() - copied);
        // INVARIANT: `load_segments` mapped every page of `[start, end)`,
        // and `data` lies within `[vaddr, vaddr + mem_size)`.
        let phys = pages[&(va & !(PAGE - 1))];
        let dst = mem::phys_to_virt(PhysAddr::new(phys)) + in_page as u64;
        // SAFETY: `dst..dst + count` stays inside one freshly mapped frame
        // (`count <= PAGE - in_page`) reachable through the physical map, and
        // the source slice is disjoint kernel-heap/image memory.
        unsafe {
            core::ptr::copy_nonoverlapping(
                segment.data.as_ptr().add(copied),
                dst.as_mut_ptr::<u8>(),
                count,
            );
        }
        copied += count;
    }
}
