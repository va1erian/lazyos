# Virtual memory: VMAs, COW, mmap

**What it is.** The per-address-space description of what memory *may become*
present, plus the Linux `mmap`/`brk`/`mprotect`/`munmap` paths built on it.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/vma.rs` | `Vma`, `Prot`, `Kind`, global per-PML4 registry, list ops |
| `kernel/src/mem/uspace.rs` | `demand_fault`, `cow_fault`, `unmap_range`, `protect_range` |
| `kernel/src/arch/idt.rs:196` | `page_fault_dispatch`: COW -> demand-zero -> `SIGSEGV` |
| `kernel/src/process/linux/mem.rs` | `sys_mmap`, `sys_munmap`, `sys_mprotect`, `sys_brk`, `sys_mremap` |
| `kernel/src/process/mod.rs:263` | Native `sbrk` (syscall 4) and VMA recording |
| `kernel/src/task/mod.rs` | `Bump` state (`task/mod.rs`): per-PML4 `brk` and `mmap_next` |

**Types** (`vma.rs`)

| Item | Values |
|---|---|
| `Prot` | `READ=1`, `WRITE=2`, `EXEC=4` (numeric values match Linux `PROT_*`) |
| `Kind` | `Anon`, `File`, `Stack`, `Heap` |
| `Vma` | page-aligned `start < end`, `prot`, `kind` |

- VMAs are keyed by PML4 **physical** address in `SPACES: Mutex<Vec<Space>>`, so
  `clone(CLONE_VM)` threads share one list without changing `Task`.
- `register` resets a recycled PML4's list; `forget` drops it at teardown;
  `clone_space` copies the parent's list on fork.

**List operations**

| Function | Semantics |
|---|---|
| `insert` | `MAP_FIXED`: overwrites covered ranges, keeps outside parts, merges adjacent equal VMAs |
| `remove` | splits partially covered VMAs; returns the clipped removed pieces |
| `find` / `find_range` | lookup by address / range, clipped |
| `protect` | splits at boundaries, sets `prot` on covered pieces, re-merges |
| `list` | snapshot for diagnostics/tests (a future `/proc/self/maps`) |

**Fault resolution** (`mem/uspace.rs`)

- Page tables are the source of truth for what is present; the VMA list for what
  may become present. `demand_fault` consults it and only materializes
  `Anon`/`Heap`/`Stack` ranges the fault's access permits. `File` ranges (the
  pages of a `PT_LOAD` segment that hold file bytes) are mapped eagerly; a
  segment's `.bss` tail past its last file page is an `Anon` range; a stack
  is mapped eagerly only where the loader wrote the start frame.
- A write fault on a present `COW_BIT` page takes `cow_fault` first; a protection
  violation is never a missing page, so it falls through to `SIGSEGV`.

**User memory layout**

Both ABIs share one layout (`process/layout.rs`) inside the private window
(`mem/layout.rs`):

| Region | Native (`process/mod.rs`) | Linux (`process/linux/elf.rs`, `process/linux/mem.rs`) |
|---|---|---|
| Image | link address, below `MMAP_BASE` | link address (static-PIE: 0), below `MMAP_BASE` |
| Heap (`brk`) | `sbrk` from `USER_HEAP_BASE = 0x60_0000` or the page after the image, up to `NATIVE_HEAP_LIMIT = MMAP_BASE` | `brk` from the page after the image, up to `BRK_LIMIT = MMAP_BASE` (16 TiB); never over another mapping |
| mmap | - | `MMAP_BASE = 0x1000_0000_0000` .. `MMAP_LIMIT = 0x7000_0000_0000`, first fit |
| MMIO (drivers) | `MMIO_BASE = 0x7000_0000_0000` .. `MMIO_END` (32 GiB) | same |
| Stack | top `STACK_TOP = 0x7f00_0000_0000`, 128 KiB, eager | top `STACK_TOP`, `limit.stack_size` (8 MiB default), demand-zero below the start frame |

- The image may not reach `MMAP_BASE` (`layout::IMAGE_RESERVED`); everything
  above is placed by the kernel. Below the stack's reservation and above
  `STACK_TOP` (512 GiB up to `USER_TOP`) nothing is mapped, so an overflow is a
  `SIGSEGV`.

- `mmap` supports `MAP_ANONYMOUS` and `MAP_FIXED` (`linux.rs:237`); it records an
  `Anon` VMA and charges per-uid `UserMemory` quota. `mprotect` privatizes COW
  pages before applying new protection. `munmap` drops VMAs and unmaps leaves.
- Native `sbrk` grows a `Heap` VMA, populated on first touch; shrinking removes
  and unmaps. It refuses growth past `NATIVE_HEAP_LIMIT` or over another
  mapping and returns `u64::MAX` on failure.

**Invariants / decisions**

- VMA granularity is one page (4 KiB); there are no huge user mappings.
- The registry keeps `Task` unchanged; moving it into an `AddressSpace` field is
  a pure refactor noted in `vma.rs`.
- `munmap`/`mprotect`/fault resolution all agree because they share this list.

**Status.** Working: COW fork, demand-zero, split/merge/protect, `mmap`/`brk`,
`mremap` (grow, shrink, and move with `MREMAP_MAYMOVE`/`MREMAP_FIXED`; one whole
VMA only, overlapping source and destination refused with `EINVAL`).
Missing: file-backed demand paging, shared mappings.
