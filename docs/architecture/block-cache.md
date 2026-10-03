# Block cache: write-back caching for ext2

**What it is.** A write-back cache of filesystem blocks inside the ext2 driver
(`libs/ext2fs`), used by every real ext2 mount in the kernel and by the host
image build. Writes stay in memory until a *commit* writes them back in a
crash-safe order, coalesced into large requests; reads are served from memory,
with read-ahead on sequential misses.

**Key files**

| Path | Role |
|---|---|
| `libs/ext2fs/src/cache/mod.rs` | `BlockCache`: slots, CLOCK eviction of clean blocks, read-ahead, dirty limit |
| `libs/ext2fs/src/cache/flush.rs` | writeback: dirty blocks sorted by (phase, block), runs coalesced into vectored requests |
| `libs/ext2fs/src/cache/roles.rs` | the five writeback phases and the metadata map read at mount |
| `libs/ext2fs/src/cache/memory.rs` | `CacheMemory`/`CachePage` (where pages come from), `CacheConfig`, `HeapMemory` |
| `libs/ext2fs/src/commit.rs` | commits, deferred frees, write-back error reporting, `writeback()` |
| `kernel/src/fs/ext2/cache.rs` | frames as cache pages, the size knob, the kernel `writeback` |
| `kernel/src/fs/flusher.rs` | the periodic flusher (every 5 s) and the memory-pressure hook |
| `kernel/src/block/virtio.rs`, `virtio/gather.rs` | vectored requests: a segment list through the 64 KiB bounce region |
| `kernel/src/block/stats.rs` | per-device request counters (`block: virtio0 reads N ... writes M ...`) |

## Why, and why in the ext2 driver

Measured before the cache (WHPX, desktop image, first boot): provisioning the
ten core packages (~30 MB into `/apps`) took **139.6 s** of `pkgd` time and
`PKGD:PROVISION:DONE` came 146.8 s after QEMU started; the disk saw 79 705
read and 52 044 write requests. Each allocated block cost about six
synchronous writes (bitmap, group descriptor, superblock, zeroing, data,
inode/indirect) and eight reads, each one a VM exit. The host benchmark
(`cargo test -p ext2fs --release bench -- --ignored --nocapture`, a 30 MB tree)
showed the same shape: 49 570 writes and 66 182 reads for 8 189 blocks.

The cache lives in the driver, not under it in the block layer, because only
the driver knows two things the crash ordering needs: which blocks are
metadata of which kind, and which blocks were just allocated. A block-layer
cache below every filesystem would have to write back in an arbitrary order
(or not at all until sync), and ext2 has no journal to repair the result. One
implementation also serves the kernel, the host image build and the host
tests. The kernel's part is small: memory, the flusher, and vectored virtio
requests.

FAT `/boot` and the read-only ATA fallback are unchanged (FAT is read-only and
small; an ext2 volume on ATA mounts read-only and uses the cache for reads).

## How it works

- **Pages.** Each cached block owns one 4 KiB page from the host's
  `CacheMemory`. The kernel hands out whole physical frames, reached through
  the physical-memory window (the 16 MiB heap only holds a 16-byte handle per
  page and the index). A failed page allocation is not an error: the cache
  recycles one of its own.
- **Size.** Kernel default: 1/32 of RAM per volume, clamped to 1-32 MiB (about
  7 MiB on the 256 MiB QEMU guest). `LAZYOS_BLOCK_CACHE_KB` fixes it at build
  time (`0` mounts uncached; touch `kernel/src/main.rs` after changing it). The
  cache takes no new frames while fewer than 1/16 of all frames are free.
  Host build: 64 MiB of heap.
- **Reads** hit the cache; a miss that continues the previous miss reads up to
  16 following uncached blocks in the same request.
- **Writes** copy into the page and mark it dirty. A block the driver just
  allocated is installed as a zeroed *fresh* page (no I/O) instead of being
  zeroed on disk.
- **Eviction** is CLOCK over clean blocks. A dirty block is never dropped.
- **Writeback** writes *every* dirty block, phase by phase, each phase sorted
  by block number and cut into runs of consecutive blocks of at most 64 KiB
  (the virtio bounce region; 1 MiB on the host), each run one vectored
  request. It runs on a commit, when `dirty_limit` (half the cache) blocks are
  dirty, and when a page is needed and every page is dirty. Nothing else ever
  writes, which is what makes the phase order hold for every byte on disk.
- **Commit** (`commit.rs`): writeback, then the deferred frees go back to their
  bitmaps, a second writeback, and a device flush.

### The writeback phases (`cache/roles.rs`)

1. **Fresh** blocks: allocated since they were last written back. Nothing
   durable points at them yet, so this is always safe, and it guarantees no
   pointer reaches the disk ahead of the bytes it points to: a crash can never
   show a file the previous owner's bytes (the reason the direct driver zeroes
   every new block before linking it).
2. **Alloc**: the group descriptor table and the block and inode bitmaps, so a
   block or inode is marked used before anything durable references it.
3. **Inodes**: the inode tables, so a directory entry never lands ahead of the
   inode it names.
4. **Content**: everything else: data overwritten in place, and directory and
   indirect blocks that were already linked.
5. **Super**: the superblock (free counters, `s_state`) last.

Frees need the opposite order (the pointer must be gone from the disk before
the bitmap bit clears), so they are **deferred to the commit**: `free_block` and
`free_inode` only record the number; the commit applies them after the first
writeback has made every detach durable. This also keeps a freed block from
being reused, and written with another file's bytes, while a durable pointer
still names it. Freed space counts as free in `statfs` immediately, and an
allocation that finds the volume full commits and retries once; more than 16 K
pending frees force a commit.

### Barriers

A few operations promise an order the phases cannot give, because it is
between two blocks of the same kind or puts a directory block ahead of an
inode. They call `Ext2::barrier` (a writeback) between their steps, which keeps
the direct driver's promise exactly:

| Operation | Barrier | Promise kept |
|---|---|---|
| rename of a file (`rename_file.rs`, confd's #407 commit) | after the new name; after the old name goes, before the link count drops | a crash shows the old or the new name (or both, link count 2), never neither, never two names on one link |
| rename of a directory (`rename.rs`) | after the new entry, before `..` moves and the old entry goes | the directory is reachable under one name at least |
| deleting a parked orphan (`orphans.rs`) | after the blocks, inode and bitmaps, before the name goes (its frees are not deferred: that order *is* the design) | the next mount's reclaim can always finish the delete, with no leak |
| truncate to a size inside a block (`truncate.rs`) | after zeroing the cut tail, before the new size | a later grow never shows the bytes that were cut |

A barrier costs one writeback (a few requests); renames and unaligned
truncates are rare next to writes. Uncached, `barrier` does nothing.

## Durability points

| Event | What happens |
|---|---|
| `fsync`, `sync`, `Vfs::sync_all` | `Ext2::flush`: commit, then the clean marker (written back and flushed last) |
| orderly power-off / reboot | `power` (syscall 21) -> `fs::sync_all` -> the same `Ext2::flush` on every mount; the power path itself is unchanged |
| every 5 s | `fs::flusher::service` (kernel task loop): commit every mount, volume stays flagged dirty; skipped (retried in 0.1 s) while the VFS is busy |
| memory pressure | the flusher, with fewer than 1/16 of frames free: commit, then every clean page is given back |
| dirty limit / cache full | a writeback inside the operation that needed room |
| unmount (`Ext2` dropped) | commit, `s_state` untouched (as before the cache) |

## Crash semantics

ext2 has no journal, so this is what a power cut can leave, and what the
repair after it does ([below](#recovery-what-is-repaired-and-how)). The
volume's `s_state` is marked dirty, durably, before the first change of a
session reaches the cache (unchanged: "dirty first"), and is only marked clean
by a sync after everything is on disk ("clean last"); so any image a crash
leaves between two syncs mounts as *not cleanly unmounted*, and is logged.

- **Loss window.** Changes since the last commit are lost: at most about 5 s
  of work (the flusher), less under write pressure (the dirty limit). Before
  the cache every write was synchronous, so a crash lost at most the operation
  in flight. A completed `fsync`/`sync` loses nothing.
- **What an interrupted writeback can leave** (the phases bound it): leaked
  blocks and inodes (marked used, unreachable); free counters in the group
  descriptors and superblock that disagree with the bitmaps; link counts and
  `i_blocks` out of step; a directory size that disagrees with its blocks; a
  directory entry naming an inode whose initialisation did not land (it reads
  as an unsupported type), or whose deletion landed ahead of the entry's
  removal (no links, a deletion time); a renamed file under both names (link
  count two, exactly as without the cache); a moved directory under both
  names, or under its new name with `..` still naming the old parent.
- **What it cannot leave**: a block or inode that is reachable and marked free,
  a block claimed by two inodes, a garbled directory block, or a file showing
  bytes that belonged to another file (or to a deleted one), a renamed file or
  moved directory under neither name, or a parked orphan the next mount
  cannot finish deleting. The host test
  `every_crash_point_keeps_the_documented_semantics` rebuilds the disk from
  every prefix of the cache's writes and checks exactly this list against the
  independent checker (its rename and orphan siblings check the last two);
  reordering any phase, turning off the deferred frees or dropping a barrier
  makes one of them fail.
- **Torn blocks.** Like the direct driver, the argument assumes a block write
  is atomic; a torn 4 KiB block (sector-level atomicity only) is outside it.
- **Within a phase** the order is by block number, not by time: two
  independent changes of the same kind (say, entries in two directories by
  two `create`s) can land in either order. Where an operation needs an order
  it uses a barrier (above).

Compared with the direct driver, a crash now loses more work (the window) and
can show the inconsistencies above for changes that were in flight together;
the direct driver's per-operation orderings ("zero, then link", "detach, then
free", the rename and orphan orders) are kept by the phases, the deferred frees
and the barriers rather than by synchronous writes. Nothing changes for a
cleanly stopped system.

### Recovery: what is repaired, and how

`Ext2::repair` (`libs/ext2fs/src/repair/`) repairs exactly the list above and
refuses the "cannot leave" list. The image build runs it from
`Ext2::recover` when an in-place update finds the OS volume unclean and the
independent checker finds problems; the volume is certified (and marked clean
by the closing flush) only if the checker then passes. It is `no_std` and
works through the driver's own cache, so the kernel can reuse it at mount.
The rules, the ones `e2fsck -p` follows where they apply:

| Found | Repair | Why |
|---|---|---|
| An entry naming an allocated inode that is dead: an unsupported type (its initialisation never landed), or no links or a deletion time (a delete whose name removal did not land) | the entry is removed, then the inode freed | the create or delete was in flight; finishing the delete or undoing the create loses only work inside the loss window |
| An entry naming a *free* inode | refused | not a crash shape (alloc lands before the inode, frees after the detach) |
| A directory under two names (a rename cut short) | the name its `..` agrees with stays, the other goes | a directory has one parent; the rename's own promise ("at least one name") holds |
| A directory with one name and a `..` naming another directory | `..` follows the name | a rename whose `..` update did not land |
| An unreachable inode that is dead, an empty file, or a directory with only `.` and `..` | freed | nothing in it to save |
| An unreachable inode with data (a file whose name did not land, a directory subtree) | linked as `/lost+found/#<ino>` (a subtree by its top directory only, its `..` repointed) | fsck's rule: the name was lost, the data need not be |
| A block marked used that nothing reaches | freed | a leak: deferred frees, or an allocation whose pointer did not land |
| A link count different from the entries naming the inode | set to the entries, in both directions | fsck's rule: a count above the entries leaks the inode at its last unlink, one below frees it while still named |
| `i_blocks`, a directory `i_size`, group free/directory counts, superblock free counts | recomputed from the blocks owned and the bitmaps | pure bookkeeping |
| A reachable block or inode marked free, a block claimed twice or by metadata, an out-of-range pointer, a garbled directory record, a directory with a hole, a bad `.`, a reserved inode owning blocks, unknown compatible features or an extended-attribute block | refused, nothing written | not a crash shape; left for a real fsck (`LAZYOS_RESET_OS=1` for the OS image) |

It runs in rounds, each from a fresh scan, with a barrier between them:
entry fixes, then the `/lost+found` links, then the frees and counts. Nothing
is freed while an entry still needs fixing, so a power cut during the repair
leaves a volume the next repair accepts. Every repair is reported
(`RepairReport`: a count and the first 16 inode or block numbers of each
kind); its `Display` is the one-line summary the build prints as
`cargo:warning=... re-certified it after repairing ...`.

Host tests: `tests/repair.rs` builds each class by hand (repaired, checker
clean, every reachable file's bytes unchanged by inode number) and each
refused class (refused, nothing written, volume stays flagged);
`tests/repair_crash.rs` cuts random workloads and every write of a rename
sequence through the cache (unflushed dirty blocks lost), recovers, and also
cuts the repair's own writes and recovers again; the fuzz entry point runs
the repair on corrupted images (never panics or loops) and on every finished
model volume (nothing to do, nothing written).

## Errors

A writeback request that fails stops the writeback on the spot (no later phase
may land ahead of it); every block it did not write stays dirty and is retried
by the next commit. Nothing is dropped. The failure marks the volume errored:

- the operation that triggered the writeback gets `EIO`-class `Invalid`
  (`Ext2Error::Io`); a write that already landed bytes reports the short count,
  and the next write meets the error;
- the next `fsync`/`sync` reports it, once, even if its own retry succeeded
  (the periodic flusher does not consume it, it logs `ext2: <dev>: writeback
  failed`);
- `s_state` written by the next clean stop carries `STATE_ERROR`, so the next
  mount logs `has recorded filesystem errors`.

With a dead disk, writers start failing once the dirty limit is reached; reads
of what they wrote keep working from the cache.

## Numbers

| Measure | Before | After |
|---|---|---|
| first-boot provisioning, sum of `pkgd` install ticks | 13 960 ticks (139.6 s) | 357-614 ticks (3.6-6.1 s, four runs on a loaded host) |
| QEMU start to `PKGD:PROVISION:DONE` | 146.8 s | 4.1-14.2 s |
| virtio requests by then (reads / writes) | 79 705 / 52 044 | ~520 / ~810 |
| bytes read / written | 311 MiB / 203 MiB | 27 MiB / 34 MiB |
| host bench, 30 MB tree: reads / writes | 66 182 / 49 570 | 58 / 759 |
| host bench: write requests per allocated block | 6.05 | 0.09 |
| host image build (`LAZYOS_RESET_OS=1`, desktop) | ~3 s | ~3 s |

## Tests

- Host (`cargo test -p ext2fs`): `tests/cache_ops.rs` (cached images
  byte-identical to direct ones with deferred frees, over tiny and roomy
  caches and random flush points; read-your-writes; a refused writeback
  reported and retried; a dead disk failing the writer; frees; read-ahead;
  shrink), `tests/cache_crash.rs` (the crash semantics above at every prefix
  of the cache's writes, renames never losing a file or directory, a parked
  delete always resumable; each fails if its phase, deferral or barrier is
  taken away), the fuzz model
  mode through an 8-block cache when bit 2 of its first byte is set, and the
  benchmark (`tests/bench.rs`). `FUZZ_CASES=<n>` scales the seeded ones.
- Kernel (`ext2_suite::block_cache`, `bcache_*`): read-your-writes and fsync
  durability, write errors reaching sync and `s_state`, a dead disk, a 4-page
  cache, periodic writeback, the power-off sync, `confd`'s commit under a power
  cut at every write request (the cache's blocks lost with it), frames returned under pressure
  and at unmount, virtio scatter/gather across request boundaries, a cached
  volume on the scratch virtio disk; soaks: a million block operations through
  eight pages, 300 remount cycles with the frame count back to baseline.
- Timing: `tools/screenshot/examples/provision_time.json` boots a fresh desktop
  image to `PKGD:PROVISION:DONE`; the `block:` lines give the request counts.
