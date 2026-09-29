# Physical memory & paging

**What it is.** The frame allocator (refcounted 4 KiB frames) and the raw paging
surface used by every address space.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/frames.rs` | `Frames` allocator and frame refcounts |
| `kernel/src/mem/uspace.rs` | User tables, COW, teardown, `demand_fault`/`cow_fault` |
| `kernel/src/mem/mod.rs` | Kernel tables, `init`, re-exports |
| `kernel/src/mem/vma.rs` | Per-address-space VMA list (see [virtual-memory.md](virtual-memory.md)) |
| `kernel/src/mem/heap.rs`, `slab.rs` | Kernel allocators (see [allocators.md](allocators.md)) |

**Allocator model** (`Frames`, `mem/frames.rs`)

- A `u32` refcount side table, one entry per 4 KiB frame up to the highest usable
  address, plus an intrusive free list threaded through free frames themselves.
- The table is carved from the first usable region at `init`; its frames are
  marked `RESERVED` (`u32::MAX`). `0` = free, `1..` = live.
- Usable regions come from `BootInfo.memory_regions`, clamped above `LOWEST_FRAME`
  (1 MiB, where the bootloader lives); at most `MAX_REGIONS` (32) regions.
- The kernel heap (16 MiB at `HEAP_START = 0x_4444_4444_0000`) is mapped last, so
  the allocator itself never needs the heap.
- `EFER.NXE` is enabled before any VMA-derived mapping, because `prot_flags` sets
  NX on non-executable pages.

**Public API**

| Function | Contract |
|---|---|
| `alloc_frame` / `alloc_zeroed_frame` | refcount 0 -> 1; returns `PhysAddr` |
| `share_frame` | +1 on a live frame (COW fork); false for dead/foreign |
| `free_frame` | -1; returns to the pool at 0; false on double/invalid free |
| `frame_refcount` / `frame_stats` | diagnostics; `FrameStats::live()` = alloc - free |
| `phys_to_virt` / `physical_offset` | via the bootloader's physical-memory mapping |
| `new_user_table` | fresh PML4 sharing kernel entries 1..512, empty entry 0 |
| `clone_user_table` | COW-share the user half; registers `vma::clone_space` |
| `map_page_in` / `switch_to` | map in a specific PML4 / load CR3 |
| `free_user_table` | tear down entry 0; drops VMA + bump registries; PML4 too |
| `cow_fault` | resolve a write fault on a `COW_BIT` page with a private copy |
| `demand_fault` | materialize a zero page for `Anon`/`Heap` VMAs only |
| `unmap_range` / `protect_range` | `munmap`/`mprotect` leaf surgery (COW privatized) |
| `user_table_frame_count` / `vma_stats` | resident pages / VSZ for tools and tests |

**COW mechanics**

- `COW_BIT = 1 << 9` (PTE bit 9, ignored by the CPU) marks a shared read-only
  page; `clone_user_table` clears `WRITABLE` and sets it on both sides while
  `share_frame` bumps refcounts.
- `cow_fault` copies the frame, maps it writable without `COW_BIT`, and releases
  the old reference, so the last user frees the frame.
- `protect_range` privatizes a COW page before changing protection; `mprotect`
  protection is per address space.

**Teardown invariants**

- Only PML4 entry 0 is walked; higher-half kernel mappings are shared and never
  freed. The PML4 frame itself is released by `free_user_table`.
- `CLONE_VM` threads share one PML4; teardown must run only when the reaped task
  is the last user (`task::reap_child` checks).
- Huge pages are logged and ignored during teardown (`free_table`), since the
  kernel maps only 4 KiB leaves in the user half.
- Frame allocator counters (`double_frees`, `invalid_frees`) are the leak/bug
  report; the boot log prints `live/allocated/freed/free`.

**Status.** Working, covered by the in-kernel soak (`mem_soak_cow_fork_churn`).
Known gaps: no frame reclaim beyond the free list, no swap, no page cache, and
the kernel heap never shrinks.
