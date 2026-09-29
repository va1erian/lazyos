# Virtual memory: VMAs, COW, mmap

**What it is.** The per-address-space description of what memory *may become*
present, plus the Linux `mmap`/`brk`/`mprotect`/`munmap` paths built on it.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/vma.rs` | `Vma`, `Prot`, `Kind`, global per-PML4 registry, list ops |
| `kernel/src/mem/mod.rs` | `demand_fault`, `cow_fault`, `unmap_range`, `protect_range` |
| `kernel/src/arch/idt.rs:196` | `page_fault_dispatch`: COW -> demand-zero -> `SIGSEGV` |
| `kernel/src/process/linux/mem.rs` | `sys_mmap`, `sys_munmap`, `sys_mprotect`, `sys_brk` |
| `kernel/src/process/mod.rs:263` | Native `sbrk` (syscall 4) and VMA recording |
| `kernel/src/task/mod.rs:324` | `Bump` state: per-PML4 `brk` and `mmap_next` |

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

**Fault resolution** (`mem/mod.rs`)

- Page tables are the source of truth for what is present; the VMA list for what
  may become present. `demand_fault` consults it and only materializes
  `Anon`/`Heap` ranges the fault's access permits. `File`/`Stack` are mapped
  eagerly.
- A write fault on a present `COW_BIT` page takes `cow_fault` first; a protection
  violation is never a missing page, so it falls through to `SIGSEGV`.

**User memory layout**

| Region | Native (`process/mod.rs`) | Linux (`process/linux/mod.rs`, `process/linux/mem.rs`) |
|---|---|---|
| Heap (`brk`) | `USER_HEAP_BASE = 0x60_0000`, `sbrk` only | `BRK_BASE = 0x0100_0000` .. `BRK_LIMIT = 0x1f00_0000` |
| mmap bump | - | `MMAP_BASE = 0x4000_0000` .. `MMAP_LIMIT = 0x7000_0000` |
| Stack | `USER_STACK_TOP = 0x80_0000`, 128 KiB | `STACK_TOP = 0x0200_0000`, 1 MiB |

- `mmap` supports `MAP_ANONYMOUS` and `MAP_FIXED` (`linux.rs:237`); it records an
  `Anon` VMA and charges per-uid `UserMemory` quota. `mprotect` privatizes COW
  pages before applying new protection. `munmap` drops VMAs and unmaps leaves.
- Native `sbrk` grows a `Heap` VMA, populated on first touch; shrinking removes
  and unmaps. It refuses growth into the stack and returns `u64::MAX` on failure.

**Invariants / decisions**

- VMA granularity is one page (4 KiB); there are no huge user mappings.
- The registry keeps `Task` unchanged; moving it into an `AddressSpace` field is
  a pure refactor noted in `vma.rs`.
- `munmap`/`mprotect`/fault resolution all agree because they share this list.

**Status.** Working: COW fork, demand-zero, split/merge/protect, `mmap`/`brk`.
Missing: file-backed demand paging, `mremap`, shared mappings, per-process cwd.
