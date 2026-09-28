# Allocators: kernel heap, slab, user bump

**What it is.** The three allocators in the tree: the kernel's general heap, the
kernel object slab allocator, and the ring-3 bump allocator.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/heap.rs` | `#[global_allocator] LockedHeap` (`linked_list_allocator`) |
| `kernel/src/mem/slab.rs` | Size-class slab allocator with owner accounting |
| `user/src/heap.rs` | Bump allocator over the `sbrk` syscall |
| `kernel/src/tests.rs` | `heap_suite`, `slab_suite` coverage |

**Kernel heap** (`heap.rs`)

- 16 MiB mapped in `mem::init` at `HEAP_START = 0x_4444_4444_0000`; backs `alloc`
  (`Vec`, `String`, `Box`) and the slab oversized fallback.
- Initialized only after the heap pages are mapped; `ALLOCATOR.lock().init(start, size)`.
- Never shrinks; growth would require mapping more frames.

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
