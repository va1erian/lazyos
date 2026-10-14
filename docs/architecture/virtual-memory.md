# Virtual memory: VMAs, COW, mmap

**What it is.** The per-address-space description of what memory *may become*
present, plus the Linux `mmap`/`brk`/`mprotect`/`munmap` paths built on it.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/mem/vma.rs` | `Vma`, `Prot`, `Kind`, global per-PML4 registry, list ops |
| `kernel/src/mem/pagefault.rs` | `demand_fault`, `cow_fault` (`Resolve`: handled, refused, out of frames) |
| `kernel/src/mem/uspace.rs` | `unmap_range`, `protect_range`, user table construction |
| `kernel/src/mem/oom.rs` | `kill_current`: a fault with no frame to give ends the process with `SIGKILL` |
| `kernel/src/arch/idt.rs:196` | `page_fault_dispatch`: COW -> demand-zero -> `SIGSEGV`, or `SIGKILL` (OOM) when no frame is left |
| `kernel/src/process/linux/mem.rs` | `sys_mmap`, `sys_munmap`, `sys_mprotect`, `sys_brk`, `sys_mremap` |
| `kernel/src/process/mod.rs:263` | Native `sbrk` (syscall 4) and VMA recording |
| `kernel/src/task/mod.rs` | `Bump` state (`task/mod.rs`): per-PML4 `brk` and `mmap_next` |

**Types** (`vma.rs`)

| Item | Values |
|---|---|
| `Prot` | `READ=1`, `WRITE=2`, `EXEC=4` (numeric values match Linux `PROT_*`) |
| `Kind` | `Anon`, `File`, `Stack`, `Heap`, `Shared` (memfd pages), `SharedAnon` |
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

**Fault resolution** (`mem/pagefault.rs`; a fault with no frame left to give is an OOM kill, `mem/oom.rs`)

- Page tables are the source of truth for what is present; the VMA list for what
  may become present. `demand_fault` consults it and only materializes
  `Anon`/`Heap`/`Stack` ranges the fault's access permits. `File` ranges (the
  pages of a `PT_LOAD` segment that hold file bytes) are mapped eagerly; a
  segment's `.bss` tail past its last file page is an `Anon` range; a stack
  is mapped eagerly only where the loader wrote the start frame.
- A write fault on a present `COW_BIT` page takes `cow_fault` first; a protection
  violation is never a missing page, so it falls through to `SIGSEGV`.
- **The leaf decides, the parents never restrict** (issue #853). A CPU takes the lowest R/W bit of
  the whole walk, so every PML4, PDPT and PD entry above a user leaf is writable and user-accessible
  (`try_map_page_in` maps with `map_to_with_table_flags`, because `map_to` would derive the parents from the
  leaf: the first read-only leaf in an untouched 2 MiB region, such as a moved COW page from `mremap` or a
  read-faulted `PROT_READ` or shared file page, made read-only parents, and the later in-place change of the
  leaf by `cow_resolve`, `mprotect` or a shared page's first store faulted again with nothing to resolve).
  An existing parent that lacks a flag has it added by the next mapping, and the in-place paths call
  `make_walk_writable` for a table an older kernel built. Tests: `mem_suite/table_parents.rs` and
  `ext2_suite/data_fds/filemap/parents.rs` assert it with `parents_writable`.

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

- `mmap` supports `MAP_ANONYMOUS` and `MAP_FIXED`; it records an
  `Anon` VMA and charges per-uid `UserMemory` quota. `MAP_SHARED` is real
  for a memfd and for anonymous memory (`process/linux/mmapfile.rs`): the
  range is mapped eagerly with the frames' `pte::SHARED` bit set (software bit
  52), so `fork` shares the frame writable instead of copy-on-write
  (`mem/cow.rs`), and a `Shared` VMA (a memfd's pages, charged to the memfd,
  see `mem/memfd.rs`) gives no quota back at `munmap`. A `MAP_PRIVATE` memfd
  mapping is a read-only COW view of the memfd's frames; `cow_fault` refuses
  a write the VMA does not permit. `mprotect` rewrites the PTE
  flags of the resident pages (a COW page that is still shared keeps its
  frame, see "Executable memory"). `munmap` drops VMAs and unmaps leaves.
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
Shared mappings of memfds and of anonymous memory work (issue #771).
File-backed `mmap` of regular files works (issue #535): the VMA names its open
file and offset (`Kind::FilePrivate`/`FileShared`, `Vma::file`/`file_off`; the
per-space file table lives in `mem/vma`) and pages fault in from a per-inode
page cache (`fs/openfile/pagecache.rs`, `mem/filemap.rs`). Private pages map
read-only copy-on-write, shared ones with `pte::SHARED`; the cache is
write-through for `write`/`ftruncate`, `read` sees stores of writable shared
mappings, and dirty pages are written back at `msync`, `munmap`, address-space
exit, `fsync` and by the periodic flusher (which never waits for a busy
volume). A shared page maps read-only on a read fault and dirty-and-writable on
the first store; a write-back write-protects every mapping of the page again,
so only changed pages are written. The address-space validators repeat a
validation when a file fault may have parked the task (`mem::filemap::
fault_epoch`). Quota is charged at `mmap`;
`PROT_NONE` file mappings are fine (demand-faulted). A truncation tears the
mappings of the dropped pages down in every address space, so the next touch
faults. A file whose last reference is dropped in the reaper (where a wait is
not allowed) parks its dirty pages for the flusher or `sync`; an unlinked
file parks nothing. **SMP:** the design assumes one CPU. The fault epoch (a
validation is correct only while no other task ran), the write-protection of
other spaces' page tables without a shootdown (`filemap::write_protect`,
`unmap_from`: another CPU's TLB would keep a writable or stale entry) and the
lock-free "no store can land while the kernel holds the cache lock" arguments
in `pagecache.rs` must all be revisited (TLB shootdown, a per-space
generation, a real lock order) before the kernel runs on more than one CPU. Limits: a page past the
end of the file is a `SIGSEGV`, not `SIGBUS`; a file truncated under a
mapping keeps already mapped pages until unmapped; writes by native services
through the native VFS are not seen by mapped pages; no read-ahead or
eviction of pages still mapped. Missing: a `PROT_NONE` shared mapping of a
memfd or anonymous memory (it would need a fault path that finds the shared
frame again) and `mremap` growth of a file mapping (`EINVAL`).

## Executable memory (JIT-style W^X)

Supported, with Linux semantics (issue #773). A JIT (JavaScriptCore, a Wasm
engine, LuaJIT, a JVM) can `mmap(RW)`, write code, `mprotect(RX)`, run it,
`mprotect(RW)`, patch and flip back, thousands of times, with other threads
executing from the page between flips; or map `RWX` once.

- **Rights.** `Prot::EXEC` maps to a PTE without `NX` (EFER.NXE is on); every
  mapping that does not ask for it is NX: `mmap` without `PROT_EXEC`, the
  `brk` heap, the Linux main stack, a segment's `.bss` and data. `mprotect`
  honours `PROT_EXEC` on any of them, as Linux does (the default is NX, not
  a prohibition). x86 has no write-only or execute-only page, so as on Linux
  any right makes the page readable; only `PROT_NONE` has nothing to
  materialize on demand. (A page that is already resident when it becomes
  `PROT_NONE` stays readable: that gap is not closed yet.)
- **W^X flips.** `sys_mprotect` updates the VMA (splitting at the range
  boundaries and re-merging what becomes equal) and the PTE of every resident
  page, with an `invlpg` per page in `mem::protect_range`. A page that is not
  resident takes the VMA's new rights at its first fault. A range may span
  several VMAs and cover them partially.
  A `prot` with a bit beyond `PROT_READ|WRITE|EXEC` is `EINVAL` whatever the
  length (`PROT_NONE` is fine); a zero length on an aligned address is then a
  successful no-op (JavaScriptCore protects 0 bytes of an empty growable
  buffer).
- **Faults.** A fetch from an NX page and a store to a read-only page are
  present-page protection faults: not demand-zero, not COW, so they reach
  `signal::deliver_fault` and are `SIGSEGV` with `si_code = SEGV_ACCERR` and
  `si_addr` the faulting address. `si_code` follows Linux, not the CPU's write
  bit: `SEGV_ACCERR` when the page is present or a VMA covers the address (a
  `PROT_NONE` range), `SEGV_MAPERR` for an address no VMA covers
  (`signal::segv_code`). A ring-3 process without a handler is ended by the
  same path.
- **`fork`.** The child inherits RX pages as shared, read-only, executable and
  COW, like every other leaf. A store to a page whose VMA is not writable is
  never answered with a copy (`cow_fault` checks the VMA): it is the access
  error above, so a forked text or JIT page cannot be written by accident.
  `mprotect(RW)` on a page still shared does not copy: the PTE stays read-only
  and COW, and the first store takes the copy, so a child that flips
  RX -> RW -> RX copies only if it writes, and its patch never reaches the
  parent. A COW page nobody else shares any more becomes private in place.
- **`mremap`.** The region's VMA keeps its protection through growing in
  place, moving (`MREMAP_MAYMOVE`) and `MREMAP_FIXED`; the PTEs move with
  their flags, so executable pages stay executable and read-only pages
  read-only.
- **TLB: single CPU.** The only flush is the local `invlpg` after each PTE
  change (`protect_range`, `cow_fault`, `unmap_range`, `remap_page`).
  LazyOS runs on one CPU, so "another thread executing the page" is another
  task on the same core, and the flush done before `mprotect` returns is
  enough for it to see the new rights (and, for a page that loses `EXEC`, to
  fault on its next fetch). A multi-CPU kernel must replace those local
  flushes with a TLB shootdown to every CPU running the address space before
  `mprotect` returns; `munmap`, `mremap` and COW need the same.
- **Instruction cache.** x86 keeps instruction fetch coherent with stores; no
  explicit flush is needed after writing code.

Tests: `kernel/src/tests/linux_compat_suite/execmem*.rs` (`execmem_*`: the W^X
cycle, RWX, every `PROT_*` combination, access-error codes and the delivered
`siginfo`, partial and spanning `mprotect`, `mremap`, stack and heap NX, fork
and COW, and a 100k-flip soak against a second thread executing the page with
the frame, VMA and mapping counts checked, and invalid `prot` values with a
zero and a page length) and the `jitmem` ABI fixture
(`tools/abi/fixtures/src/jitmem.rs`: a tiny JIT in ring 3 with real
`SIGSEGV` recovery, including a fetch after an RX -> RW flip, which fails if
the TLB keeps a stale execute right; `python tools/abi/run.py --only jitmem`).
