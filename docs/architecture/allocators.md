# Allocators: kernel heap, slab, user bump

**What it is.** The three allocators in the tree: the kernel's general heap, the
kernel object slab allocator, and the ring-3 bump allocator.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/heap.rs` | `#[global_allocator] IrqSafeHeap(LockedHeap)` (`linked_list_allocator`, lock held with interrupts off, issue #382) |
| `kernel/src/mem/slab.rs` | Size-class slab allocator with owner accounting |
| `libs/membook/src/slab.rs` | The bookkeeping both slabs build on: size classes, the intrusive free list (`Class`), owner ledgers; host-tested with a seeded soak and run under Miri (`.github/workflows/miri.yml`, issue #485) |
| `user/src/heap.rs` | Bump allocator over the `sbrk` syscall |
| `kernel/src/tests/heap_suite.rs`, `kernel/src/tests/slab_suite.rs` | `heap_suite`, `slab_suite` coverage |

**Kernel heap** (`heap.rs`)

- Lives at `HEAP_START = 0xffff_c000_0000_0000` (its own PML4 entry in the
  kernel half, 512 GiB of span); backs `alloc` (`Vec`, `String`, `Box`) and the
  slab oversized fallback.
- `mem::init` maps `limits::heap_initial_bytes(RAM)` (1/32 of RAM, 16..64 MiB)
  and initializes the allocator over it.
- **Grows on demand**: an allocation that finds no hole maps at least 4 MiB
  more at the top (`mem::map_kernel_range`, frames from the frame allocator)
  and extends the free list, then retries, up to `limit.heap_max` (half of RAM
  by default; docs/architecture/limits.md). Past the ceiling, or with RAM
  exhausted, the allocation fails: fallible callers (`try_reserve`, the
  `fs::fallible` helpers) get an error, infallible ones hit the alloc error
  handler as before. The growth path runs inside the same interrupts-off
  section, with the heap lock released and only `FRAMES` taken.
- Every address space sees growth at once: the heap's PML4 entry exists
  before the first address space is created and is copied into each one.
- Never shrinks (the linked-list allocator cannot give memory back); the
  high-water mark is what the kernel once needed. `heap_stats()` reports
  `total`, `used`, `free`, `max`, `growths` and `grow_failures`.
- **Small objects** (P6.4): requests of up to 2 KiB (size or alignment) are
  served from the heap's own slab classes (`slab::Class`, 32 B to 2 KiB, a
  separate set from the typed-object slabs below): a free-list pop or push
  under one interrupts-off lock instead of a first-fit walk. A slab is one
  page-aligned page allocated from the list (growing the heap like any
  allocation), so the heap's span, ceiling and frame use are unchanged; a
  page stays with its class once carved. Every layout a class fits is served
  by that class, so `dealloc` finds the class from the layout. `heap_stats()`
  counts a slab page's free slots as free. Lock order: the slab lock, then
  the list's.
  Tests: `heap_slab_suite` (placement, alignment, exact accounting, a
  2M-operation soak).

**Slab allocator** (`slab.rs`, issue #61)

| Property | Value |
|---|---|
| Classes | `CLASSES = [32, 64, 128, 256, 512, 1024, 2048, 4096]` bytes |
| Largest slab request | `MAX_SLAB_SIZE = 4096` |
| Slabs per class | one 4 KiB frame carved into equal slots |
| Free list | intrusive: a free slot's first word is the next link |
| Owners | `MAX_OWNERS = task::MAX_TASKS` (slot 0 = kernel) |

API: `alloc(class)` -> zeroed `NonNull<u8>`, `dealloc(class, ptr)` (unsafe),
`alloc_bytes(bytes)` / `dealloc_bytes(bytes, ptr)` for size-agnostic callers,
`stats()` -> `SlabStats`, `charge`/`uncharge`/`owner_stats` for per-slot kernel
memory ledgers. No init call: state is a const-initialized `Mutex<Slab>`.

Decisions and limits:

- Request > 4096 bytes takes the oversized heap fallback (`alloc_zeroed` with
  32 B alignment), accounted separately. This path needs the heap; the slab path
  is usable before `heap::init`.
- Frames are never returned to the frame allocator; classes keep grown slabs
  (bounded, kernel object counts plateau). Slab shrinking is a follow-up.
- `dealloc` is unsafe: a wrong class or double free corrupts the free list; only
  a class with zero live slots detects and counts a double free.
- `charge`/`uncharge` are the enforcement hook for quotas, but handle/channel/VMA
  call sites are not wired yet (issue #61 follow-up); the suite exercises the API.
- Lock order: `SLAB` is a leaf; the heap fallback is called outside it, so no
  lock cycle exists.

**User bump allocator** (`user/src/heap.rs`)

- `CHUNK = 64 KiB` requested from the kernel per refill via `sys::sbrk`.
- `dealloc` is a no-op: memory is reclaimed only when the program exits. Long
  loops must reuse buffers (the userspace Messenger API exposes `*_with` variants
  for exactly this).
- `#[alloc_error_handler]` prints the failing request size without allocating and
  exits with status 1.

**Invariants**

- Kernel object memory is zeroed on allocation in both allocators, so kernel
  structs never leak a previous occupant's data.
- The user heap is single-threaded by assumption (`unsafe impl Sync`); LazyOS user
  programs do not share an address space across threads yet.

**Status.** Working and tested (`heap_vec_integrity`, six `slab_*` tests,
quota tests). Per-uid kernel-memory quota is enforced on shared buffers today;
slab call sites migrate as subsystems land.
