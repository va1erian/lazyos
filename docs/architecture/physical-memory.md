# Physical memory & paging

**What it is.** The frame allocator (refcounted 4 KiB frames) and the raw paging
surface used by every address space.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/frames.rs` | `Frames` allocator and frame refcounts |
| `libs/membook/src/frames.rs` | The refcount rules (`shared`/`dropped`), the free chain and the counters (`Ledger`), over a `FrameMemory` trait the kernel implements on the physical-memory mapping (`PhysTable`); host-tested with a model soak and under Miri (issue #485) |
| `kernel/src/mem/dma.rs` | Boot-time contiguous DMA pool, stats and test ordering log |
| `kernel/src/mem/uspace.rs` | User tables, COW, teardown, `demand_fault`/`cow_fault` |
| `kernel/src/mem/mod.rs` | Kernel tables, `init`, `map_kernel_range`, re-exports |
| `kernel/src/mem/regions.rs` | The usable regions: firmware map merged, sorted, page aligned |
| `kernel/src/mem/layout.rs` | The shared virtual layout (user window, shared-buffer window, kernel half) |
| `kernel/src/mem/vma.rs` | Per-address-space VMA list (see [virtual-memory.md](virtual-memory.md)) |
| `kernel/src/mem/heap.rs`, `slab.rs` | Kernel allocators (see [allocators.md](allocators.md)) |

**Allocator model** (`Frames`, `mem/frames.rs`)

- A `u32` refcount side table, one entry per 4 KiB frame up to the highest usable
  address, plus an intrusive free list threaded through free frames themselves.
- The table is carved from the first usable region at `init`; its frames are
  marked `RESERVED` (`u32::MAX`). `0` = free, `1..` = live.
- Usable regions come from `BootInfo.memory_regions` through
  `Regions::gather` (`mem/regions.rs`): any order, overlapping or touching
  entries are merged, starts rounded up and ends down to a frame, everything
  below `LOWEST_FRAME` (1 MiB, where the bootloader lives) dropped. At most
  `MAX_REGIONS` (128) disjoint regions are kept; past that the smallest are
  dropped and the lost KiB logged (`mem: ... left out`), never a large one.
  RAM above the 4 GiB PCI hole is just another region: the refcount table
  covers frames up to the highest usable address, and the bootloader's
  physical map covers all of it (`mem_high_frames_reachable` checks a frame
  above 4 GiB when the guest has one).
- A contiguous **DMA pool** (issue #241) is reserved at `init` from the memory
  map: `limits::dma_pool_bytes(RAM)` (1/32 of RAM, 16..64 MiB, never more than
  1/8 of RAM; docs/architecture/limits.md), below 4 GiB, clear of the
  refcount table.
  Its frames stay in the refcount table but are marked `RESERVED` while free, so
  `pop_free` skips them; they are excluded from `total`/`free` like the metadata
  table. `Frames::release` routes a pool frame that reaches refcount zero back
  to the pool bitmap instead of the general free list, so DMA traffic never
  perturbs a frame `live()` delta. The pool bitmap is protected by the frame
  allocator lock itself, so the only documented order is `REGISTRY -> FRAMES`.
- The kernel heap (`HEAP_START = 0xffff_c000_0000_0000`, PML4 entry 384) is
  mapped last, so the allocator itself never needs the heap: an initial
  `limits::heap_initial_bytes(RAM)` (16..64 MiB), then grown on demand by
  `map_kernel_range` up to `limit.heap_max` (see [allocators.md](allocators.md)).
- `EFER.NXE` is enabled before any VMA-derived mapping, because `prot_flags` sets
  NX on non-executable pages.

**Public API**

| Function | Contract |
|---|---|
| `alloc_frame` / `alloc_zeroed_frame` | refcount 0 -> 1; returns `PhysAddr` |
| `share_frame` | +1 on a live frame (COW fork); false for dead/foreign |
| `free_frame` | -1; returns to the pool at 0; false on double/invalid free |
| `frame_refcount` / `frame_stats` | diagnostics; `FrameStats::live()` = alloc - free |
| `dma_alloc` / `dma_stats` | contiguous zeroed DMA pool run; pool free-space snapshot |
| `phys_to_virt` / `physical_offset` | via the bootloader's physical-memory mapping |
| `new_user_table` | fresh PML4 sharing entries `USER_PML4_ENTRIES..512` with the kernel, empty private window |
| `clone_user_table` | COW-share the private window; registers `vma::clone_space` |
| `map_page_in` / `switch_to` | map in a specific PML4 / load CR3 |
| `free_user_table` | tear down the private window; drops VMA + bump registries; PML4 too |
| `cow_fault` | resolve a write fault on a `COW_BIT` page with a private copy |
| `demand_fault` | materialize a zero page for `Anon`/`Heap`/`Stack` VMAs only |
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

- Only the private window (PML4 entries `0..USER_PML4_ENTRIES`, 255) is
  walked; the shared-buffer window (entry 255) and the kernel half are the
  kernel's and never freed. The PML4 frame itself is released by
  `free_user_table`.
- `CLONE_VM` threads share one PML4; teardown must run only when the reaped task
  is the last user (`task::reap_child` checks).
- Huge pages are logged and ignored during teardown (`free_table`), since the
  kernel maps only 4 KiB leaves in the user half.
- Frame allocator counters (`double_frees`, `invalid_frees`) are the leak/bug
  report; the boot log prints `live/allocated/freed/free`.

**Virtual layout** (`mem/layout.rs`)

| PML4 entries | Range | What |
|---|---|---|
| 0..=254 | `0 .. 0x7f80_0000_0000` | Private user window (~127.5 TiB), per address space; the process layout inside it is in [processes.md](processes.md) |
| 255 | `0x7f80_0000_0000 ..` | Shared-buffer window (`ipc::shared_va`), copied from the kernel's table |
| 256..=383 | `0xffff_8000_0000_0000 ..` | Bootloader mappings: kernel image, boot stack, boot info, framebuffer, physical map (`BootloaderConfig.mappings.dynamic_range_*`) |
| 384 | `0xffff_c000_0000_0000 ..` | Kernel heap (512 GiB span) |

`mem::init` stops the boot if the bootloader left anything in entries
1..=255 of the kernel's table (`check_kernel_table`), since an address space
would not inherit it. The kernel image is linked position-independent and
placed at `0xffff_8000_0000_0000`, so symbolize a fault with
`addr2line -e <kernel> <rip - 0xffff800000000000>`.

**Status.** Working, covered by the in-kernel soak (`mem_soak_cow_fork_churn`)
and `mem_suite::layout` (hostile memory maps, a user window spanning many
PML4 entries, frames above 4 GiB, the lazy Linux stack).
Known gaps: no frame reclaim beyond the free list, no swap, no page cache, and
the kernel heap never shrinks.
