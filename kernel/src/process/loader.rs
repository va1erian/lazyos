//! The static ELF64 segment loader.
//!
//! An executable is attacker-controlled input: any user who can write a file
//! to `/tmp` and set its exec bit reaches this code through `execve`. So the
//! loader works in two phases. [`plan`] validates every `PT_LOAD` header
//! against the address-space layout without mapping anything; only a plan
//! that passed is handed to [`map_plan`]. That keeps the arithmetic on
//! untrusted `p_vaddr`/`p_memsz` in one small, testable place.
//!
//! Nothing here holds the whole file: the [`Image`] is read header first,
//! then segment by segment in [`CHUNK`]-sized pieces copied straight into the
//! new frames. Pages that hold file bytes are mapped eagerly; a segment's
//! `.bss` tail beyond its last file page is an `Anon` VMA, zero-filled on
//! first touch like any anonymous memory. So the eager cost of a load is
//! bounded by the file's size, not by what its headers claim, and there is no
//! cap on image size or segment count beyond the layout itself.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use x86_64::{PhysAddr, VirtAddr};

use super::elfhdr::{self, Headers, PF_W, PF_X, PT_LOAD};
use super::image::Image;
use crate::mem;
use crate::mem::vma::{Kind, Prot};

/// First address an image may not reach: the top of the private user window.
pub const USER_LIMIT: u64 = mem::USER_TOP;

/// Bytes read from the image per copy step: bounds the loader's heap use and
/// keeps the number of filesystem calls for a large program small.
pub const CHUNK: usize = 128 * 1024;

const PAGE: u64 = 4096;

/// The load failures that mean frames or page-table pages ran out (`ENOMEM`):
/// callers classify a reason with [`is_out_of_memory`], by exact match, so a
/// new image-validation message can never be mistaken for one.
pub const OUT_OF_MEMORY: &str = "out of memory";
pub const WIDEN_FAILED: &str = "failed to widen shared page";
pub const MAP_SEGMENT_FAILED: &str = "failed to map segment";
pub const MAP_PAGE_FAILED: &str = "failed to map user page";

/// Whether a loader reason is frame exhaustion rather than a bad image.
pub fn is_out_of_memory(reason: &str) -> bool {
    [
        OUT_OF_MEMORY,
        WIDEN_FAILED,
        MAP_SEGMENT_FAILED,
        MAP_PAGE_FAILED,
    ]
    .contains(&reason)
}

/// What a successful load produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Loaded {
    pub entry: u64,
    /// Page-aligned end of the highest segment: where a heap may begin.
    pub end: u64,
    /// `AT_PHDR`, `AT_PHENT`, `AT_PHNUM` for the start stack.
    pub phdr: u64,
    pub phent: u16,
    pub phnum: u16,
}

/// One validated `PT_LOAD` segment.
#[derive(Clone, Copy)]
struct Segment {
    vaddr: u64,
    /// Byte end of the in-memory image (`vaddr + p_memsz`).
    vend: u64,
    /// Page-aligned `[start, end)` covering `[vaddr, vend)`.
    start: u64,
    end: u64,
    prot: Prot,
    offset: u64,
    filesz: u64,
    /// `[lazy_start, lazy_end)` is the demand-zero `.bss` tail (empty when
    /// equal); every other page of `[start, end)` is mapped eagerly.
    lazy_start: u64,
    lazy_end: u64,
}

/// Validate `headers` against the file length and the layout.
///
/// `reserved` lists half-open windows (heap, mmap area, stack...) the image
/// must stay out of, because the kernel maps those itself.
fn plan(
    headers: &Headers,
    file_len: u64,
    reserved: &[(u64, u64)],
) -> Result<Vec<Segment>, &'static str> {
    let mut segments: Vec<Segment> = Vec::new();
    for header in headers.phdrs.iter().filter(|ph| ph.kind == PT_LOAD) {
        let (vaddr, mem_size, file_size) = (header.vaddr, header.memsz, header.filesz);
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
            return Err("segment outside the loadable range");
        }
        if reserved.iter().any(|&(lo, hi)| start < hi && lo < end) {
            return Err("segment overlaps reserved region");
        }
        let file_end = header
            .offset
            .checked_add(file_size)
            .ok_or("segment data range")?;
        if file_end > file_len {
            return Err("segment data out of file");
        }
        let mut prot = Prot::READ;
        if header.flags & PF_W != 0 {
            prot = prot | Prot::WRITE;
        }
        if header.flags & PF_X != 0 {
            prot = prot | Prot::EXEC;
        }
        segments.try_reserve(1).map_err(|_| OUT_OF_MEMORY)?;
        segments.push(Segment {
            vaddr,
            vend,
            start,
            end,
            prot,
            offset: header.offset,
            filesz: file_size,
            // In range: `vaddr + filesz <= vend`, already checked.
            lazy_start: (vaddr + file_size + PAGE - 1) & !(PAGE - 1),
            lazy_end: end,
        });
    }
    // Sorted, distinct segments may share a boundary page but never a byte:
    // one pass over neighbours instead of a pairwise check.
    segments.sort_unstable_by_key(|segment| segment.vaddr);
    for index in 1..segments.len() {
        let (before, after) = (segments[index - 1], segments[index]);
        if after.vaddr < before.vend {
            return Err("overlapping segments");
        }
        // A last page the next segment also uses is mapped eagerly, so the
        // two segments' protections can be merged on one present page.
        if after.start < before.end {
            let shared = before.end - PAGE;
            let previous = &mut segments[index - 1];
            previous.lazy_end = previous.lazy_end.min(shared);
        }
    }
    if !segments
        .iter()
        .any(|seg| headers.entry >= seg.vaddr && headers.entry < seg.vend)
    {
        return Err("entry point outside loaded segments");
    }
    Ok(segments)
}

/// Map a program's `PT_LOAD` segments into `table` and describe the result.
///
/// Pages are mapped with the protection the ELF header asks for: read always,
/// write only for `PF_W`, execute only for `PF_X`. Each segment is recorded as
/// a `File` VMA (its lazy `.bss` tail as `Anon`) so `munmap`/`mprotect` and
/// diagnostics see the same layout the hardware does. A page shared by two
/// segments gets the union of their protections (PTE and VMA alike), so
/// neither segment faults on its own accesses.
///
/// On error `table` may hold a partial image; the caller owns it and frees it
/// with [`mem::free_user_table`].
pub fn load_segments<I: Image + ?Sized>(
    table: PhysAddr,
    image: &I,
    reserved: &[(u64, u64)],
) -> Result<Loaded, &'static str> {
    let headers = elfhdr::read(image)?;
    let segments = plan(&headers, image.len(), reserved)?;
    map_plan(table, image, &segments)?;
    Ok(Loaded {
        entry: headers.entry,
        end: segments.iter().map(|seg| seg.end).max().unwrap_or(0),
        phdr: elfhdr::phdr_address(&headers),
        phent: elfhdr::PHDR_SIZE,
        phnum: headers.phdrs.len() as u16,
    })
}

/// Map and fill every planned segment.
fn map_plan<I: Image + ?Sized>(
    table: PhysAddr,
    image: &I,
    segments: &[Segment],
) -> Result<(), &'static str> {
    // Page base -> backing frame; a map so per-page lookups stay logarithmic.
    let mut pages: BTreeMap<u64, u64> = BTreeMap::new();
    // Current protection per eager page, and the pages two segments share.
    let mut prots: BTreeMap<u64, Prot> = BTreeMap::new();
    let mut shared: Vec<u64> = Vec::new();
    let mut chunk = Vec::new();
    chunk.try_reserve_exact(CHUNK).map_err(|_| OUT_OF_MEMORY)?;
    chunk.resize(CHUNK, 0u8);

    for segment in segments {
        // The file pages, then a last page shared with the next segment.
        let tail = segment.lazy_end.max(segment.lazy_start);
        for range in [segment.start..segment.lazy_start, tail..segment.end] {
            for va in range.step_by(PAGE as usize) {
                map_eager_page(table, va, segment.prot, &mut pages, &mut prots, &mut shared)?;
            }
        }
        copy_segment(image, &pages, segment, &mut chunk)?;
        mem::vma::insert(table, segment.start, segment.end, segment.prot, Kind::File);
        if segment.lazy_start < segment.lazy_end {
            mem::vma::insert(
                table,
                segment.lazy_start,
                segment.lazy_end,
                segment.prot,
                Kind::Anon,
            );
        }
    }
    // Segment VMAs were inserted whole; restate the shared pages with the
    // union the PTEs now carry.
    for va in shared {
        mem::vma::insert(table, va, va + PAGE, prots[&va], Kind::File);
    }
    Ok(())
}

/// Back `va` with a zeroed frame, or widen the protection of a page an
/// earlier segment already mapped.
fn map_eager_page(
    table: PhysAddr,
    va: u64,
    prot: Prot,
    pages: &mut BTreeMap<u64, u64>,
    prots: &mut BTreeMap<u64, Prot>,
    shared: &mut Vec<u64>,
) -> Result<(), &'static str> {
    if let Some(current) = prots.get_mut(&va) {
        let union = *current | prot;
        if union != *current {
            if !mem::protect_range(table, va, va + PAGE, union) {
                return Err(WIDEN_FAILED);
            }
            *current = union;
        }
        shared.push(va);
        return Ok(());
    }
    let phys = mem::alloc_zeroed_frame().ok_or(OUT_OF_MEMORY)?;
    if !mem::map_page_in(table, VirtAddr::new(va), phys, mem::prot_flags(prot)) {
        mem::free_frame(phys);
        return Err(MAP_SEGMENT_FAILED);
    }
    pages.insert(va, phys.as_u64());
    prots.insert(va, prot);
    Ok(())
}

/// Stream a segment's file bytes into its mapped frames, [`CHUNK`] at a time.
fn copy_segment<I: Image + ?Sized>(
    image: &I,
    pages: &BTreeMap<u64, u64>,
    segment: &Segment,
    chunk: &mut [u8],
) -> Result<(), &'static str> {
    let mut copied = 0u64;
    while copied < segment.filesz {
        let count = (segment.filesz - copied).min(chunk.len() as u64) as usize;
        let piece = &mut chunk[..count];
        image.read_exact_at(segment.offset + copied, piece)?;
        copy_to_pages(pages, segment.vaddr + copied, piece);
        copied += count as u64;
    }
    Ok(())
}

/// Copy `bytes` to user address `va` through the frames in `pages`.
fn copy_to_pages(pages: &BTreeMap<u64, u64>, mut va: u64, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        // A big program is thousands of pages (`arch::irq_window`).
        crate::arch::irq_window::poll_point();
        let in_page = (va & (PAGE - 1)) as usize;
        let count = (PAGE as usize - in_page).min(bytes.len());
        // INVARIANT: every page holding file bytes is eager (`lazy_start` is
        // the first page past the file bytes), and `map_plan` mapped it.
        let phys = pages[&(va & !(PAGE - 1))];
        let dst = mem::phys_to_virt(PhysAddr::new(phys)) + in_page as u64;
        // SAFETY: `dst..dst + count` stays inside one freshly mapped frame
        // (`count <= PAGE - in_page`) reachable through the physical map, and
        // the source is the loader's own chunk buffer.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.as_mut_ptr::<u8>(), count);
        }
        va += count as u64;
        bytes = &bytes[count..];
    }
}
