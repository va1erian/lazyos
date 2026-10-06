# Filesystem: VFS, ramfs, FAT, ext2

**What it is.** The VFS core (mounts, path resolution, permissions, caches) and
its three backends: a read-write ext2 driver, a read-only FAT12/16 driver, and
an in-memory ramfs mounted at `/tmp`.

**Paths.** Every well-known path (`/tmp`, `/data`, the confd store, installed
apps, home directories, `/docs`) and every file the image build places
(`fhs::bin::INIT` = `/system/bin/init`, `fhs::etc::PASSWD`,
`fhs::system::PACKAGES_INDEX`, ...) is a constant in `libs/fhs`, with its target value
from [`filesystem-plan.md`](../filesystem-plan.md) in the doc comment, so the
filesystem overhaul changes a constant instead of chasing literals. Never write
one as a string literal: `python tools/fhs/check_literals.py` (run by CI, tested
by `test_check_literals.py`) fails on a literal outside `libs/fhs`. A deliberate
exception goes in `tools/fhs/allowlist.txt`; test code, comments, generated
files are exempt; byte strings are checked too, so a spawn names its program
by its `fhs::bin` constant (`sys::spawnv`). Linux ABI
synthetic paths (`/dev`, `/proc`, `/etc`, `/bin`) stay in `process/linux`.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/fs/mod.rs` (+ `abi_attr.rs`) | Init/mount, kernel-side `read`/`abi_*` entry points (`abi_setattr*` in `abi_attr.rs`) |
| `kernel/src/fs/vfs.rs` (+ `vfs/{filesystem,meta,path,cache}.rs`) | `Vfs`, resolution; `Filesystem` trait, `Path`, `Id`, permissions and the dentry/inode caches in the submodules |
| `kernel/src/fs/vfs/{attr,setattr}.rs` | Timestamps, the filesystem clock, `SetAttr`; the `chmod`/`chown`/`utimensat` rules (issue #345) |
| `kernel/src/fs/ramfs.rs` (+ `ramfs/{node,capacity,space}.rs`) | In-memory tree; root inode 1 (issue #98); a node and its attributes in `node.rs`; the byte and node caps, filesystem-wide and per owner, in `space.rs` (issue #265: a node and its bytes are charged to its owner, one non-root uid may hold half of either cap, `chown` moves the charge, root is bound only by the filesystem caps) |
| `kernel/src/fs/fat/` | Read-only FAT12/16 on the boot volume (`dir.rs` directory walker, `lfn.rs` long names, `resolve.rs` paths + cache) |
| `libs/ext2fs/` (`no_std` + `alloc`; `lib.rs`, `open`, `io`, `layout`, `blocks`, `indirect`, `truncate`, `state`, `dir`, `attr`, `orphans`, `rmdir`, `file_io`, `links`, `rename`, `readdir`, `format`, `populate`) | Read/write ext2 rev 0/1 (issues #99, #333, #345), the formatter and the image populator, behind a `BlockIo` seam; host tests, a soak and a fuzz entry point (F2) |
| `kernel/src/fs/ext2.rs`, `ext2/fsimpl.rs` | The kernel adapter: `BlockIo` for a `BlockDevice`, the VFS clock and serial lines, the `Filesystem` impl, error and metadata conversion, and the `hidden::PREFIX` orphan rule |
| `kernel/src/fs/overlay.rs` | Copy-up overlay for the Linux ABI root (issue #136) |
| `kernel/src/fs/openfile.rs` | Open files on `/data` for Linux descriptors: in-place I/O, follow renames, unlink-while-open (issue #334) |
| `libs/fhs/` | Every well-known path as a constant (`bin`, `etc`, `share`, `system`, `mount`, `state`, `boot`, `docs`) |
| `kernel/src/fs/vfs/mountops.rs` | Whole-mount operations: `flush`, `sync_all`, `statfs`, `mount_point` |

**VFS semantics** (`vfs.rs`, issue #98)

- The resolver is **symlink-free**: a path is just directory entries. `Path`
  folds `.`/`..` lexically and clamps `..` at the root. The VFS itself always
  resolves from `/`: relative names are made absolute one layer up, against the
  per-task working directory (see "Working directory" below).
- Mounts are `(point, Filesystem)`; resolution picks the **longest** mount-point
  prefix, so `/tmp/notes` lands in ramfs while `/tmp2` stays on the root volume.
- Every entry point takes an `Id { uid, gid }` from
  `ipc::credentials` (`Id::current`); ancestors need `EXECUTE` (search) and the
  final node the operation's `READ`/`WRITE` mask. Root (uid 0) bypasses bits
  ([security-model.md](../security-model.md) section 4.1), except that
  executing a regular file needs at least one `x` bit, as on Linux. The sticky bit
  restricts `unlink`/`rename` to root, the directory owner, or the entry owner.
- Caches: `(mount, path) -> ino` dentry map plus `(mount, ino) -> Meta` inode
  map; mutations invalidate the path, its inode, and cached descendants.
  `cache_stats()` exposes hits/misses for tests.
- `Filesystem` trait: `name`, `lookup`/`stat`, `read`, `write`, `truncate`,
  `setattr`, `create`, `mkdir`, `unlink`, `rmdir`, `rename`, `readdir`,
  `flush`, `statfs` (`StatFs`: magic, block size, block/inode totals and free;
  default `NotSupported`, implemented by ext2, ramfs and the overlay). The VFS
  checks permissions before calling in, so a backend only enforces what is
  intrinsic (e.g. FAT returns `ReadOnly`). `FsError` variants map to errno in
  the Linux layer (`NotPermitted` is `EPERM`, a missing ownership; `Access` is
  `EACCES`, a missing permission bit).

**Attributes** (`vfs/{attr,setattr}.rs`, issue #345)

`Meta` carries `times: Times` (`atime`, `mtime`, `ctime`, whole seconds as
`time_t`). Every backend stamps from one clock, `vfs::now()` (UTC wall time: the
CMOS RTC sampled at boot plus PIT uptime, see `wallclock.rs`), so a write and a `touch` agree; reads never move `atime`
(every mount behaves as `noatime`). FAT and the fabricated ABI entries report
zero.

Mode, owner and times change through **one** trait operation,
`Filesystem::setattr(path, &SetAttr)`. `SetAttr` is the change set: each field
is an `Option`, and `Some` means "in the set", so a value can never be present
without being selected (the set-mask and the values are one thing). The VFS
entry points take an `AttrRequest` (`Mode`, `Owner { uid, gid }`, `Times {
atime, mtime }` with `Stamp::Now`/`Stamp::At`) and turn it into an authorized
`SetAttr` before any backend is called (`vfs::authorize`):

- `chmod`: owner or root, else `NotPermitted`. A caller outside the file's
  group has setgid dropped silently (Linux does the same).
- `chown`: only root changes the uid (the owner may "set" it to itself); the
  owner may set the gid to their own group (no supplementary groups yet) or
  keep it. A regular file loses setuid *and* setgid, root's chown included;
  a directory keeps them. `chown(-1, -1)` changes nothing.
- `utimensat`: both stamps "now" (`touch`) needs ownership or write
  permission; an explicit time, or touching one stamp only, needs ownership.
  Both `UTIME_OMIT` changes nothing.
- every change also sets `ctime` to now.

`Vfs::setattr` needs search on the ancestors like any lookup;
`Vfs::setattr_open` (`fchmod`, `fchown`, `futimens`) skips that walk because the
descriptor was already opened, and applies only the request's own rule.
Afterwards the path and everything cached below it are invalidated (even on
failure: the overlay may have copied a subtree up, renumbering it) and the
backend's fresh `Meta` is cached.

Backends: ramfs stores the fields as given (a write or truncate moves `mtime`
and `ctime`); the overlay copies the node up first, a directory with its
subtree as for any write, and a copy-up keeps the lower node's mode, owner and
times; FAT answers `ReadOnly`; ext2 (`ext2/attr.rs`) validates the whole change
(ids must fit the 16-bit `i_uid`/`i_gid`, else `Invalid`), then rewrites the
one inode through `write_inode`, so the volume is marked dirty before the
inode reaches the disk and a stop leaves the old or the new attributes, never
a torn inode behind a clean flag. ext2 times are 32-bit: values outside
`0..=i32::MAX` (1970 to 2038) are clamped, as Linux clamps a time a filesystem
cannot hold.

**Mounting** (`mod.rs`)

1. `block::init()` probes devices; each device is tried as FAT
   (`Fat16::open(device)`), then as ext2 (`Ext2::open(device)`). Both readers
   keep the device they were given, so probing one disk never reads another
   (`#244`).
2. The first volume becomes `/`; `mounted` reports whether a volume was found.
3. A fresh `RamFs` is always mounted at `/tmp`, so the VFS is usable even with
   no disk volume.
4. `mount_data_volume` then probes the *remaining* devices (never the root
   device, and never assuming which bus the data disk is on) and mounts the first
   ext2 one read/write at `/data`, in both the native and the Linux ABI table,
   logging `fs: mounted <dev> at /data`. No such device is not an error. This is
   the one place it is mounted; there is no mount syscall.
5. `mount_device(point, device)` is the named-device mount surface (tests).

**Configured layout, flags and partitions** (`fs/{mounts,bootcfg}.rs`,
`vfs/flags.rs`, `block/partition.rs`; plan F1). The steps above are the
*legacy* layout and stay the fallback. If `lazyos.cfg` on the FAT boot volume
names an ext2 root, `mounts::build` instead mounts, in both tables and with the
same volume instances (no overlay):

| Mount | Volume | Flags |
|---|---|---|
| `/` | ext2 whose `s_uuid` equals `root=UUID=...` (any device or partition) | `root_flags`, plus `ro` if the device cannot be written |
| `/boot` | the FAT boot volume | `ro,noexec,nosuid` |
| `/transient`, `/tmp` | one ramfs, sticky `1777` root | none |
| `/home` | optional ext2 chosen by `home=LABEL=..`/`home=UUID=..` | `home_flags` + `nosuid`; absent volume is a log line, and the request is kept for a USB stick that appears later ([usb-storage.md](usb-storage.md): `fs::late`, mounted in both tables once `usbd` serves it) |

There is no `/data` probe in this mode. A config naming a root that is not
found logs `fs: root <uuid> not found; booting the legacy layout` and boots the
legacy layout, which is also the recovery boot. `lazyos.cfg` is `key=value`
lines (`root`, `home`, `root_flags`, `home_flags`; `#` comments), read through
the FAT backend before any mount, at most 4 KiB, UTF-8, no duplicate keys; any
violation ignores the whole file (`fs: lazyos.cfg ignored: <reason>`).
`bootcfg::parse` is pure so the suite can fuzz it by hand.

`Vfs::mount` takes `MountFlags { ro, noexec, nosuid }`. `ro` makes every
mutating entry point (`write`, `truncate`, `setattr`, `create`, `mkdir`,
`unlink`, `rmdir`, `rename`) answer `ReadOnly` before the backend is called.
`noexec` is enforced by native spawn (`-EACCES`) and Linux `execve` (`EACCES`)
through `fs::mount_flags` / `fs::abi_mount_flags`, checked first. After it
both check `EXECUTE` on the file (`process/exec_perm.rs`): a file without an
`x` bit for the caller (root included) or a directory is `EACCES`, so a `0644`
file someone wrote cannot be started by init or any root service. `nosuid` is recorded and
reported only (no setuid-on-exec exists yet). `/proc/mounts` and `mountinfo`
show the flags. `Vfs::readdir` appends the last component of each mount point
directly below the directory, so `/boot`, `/home`, `/transient` are listed
without the Files app special-casing them.

Whole-disk MBRs are parsed once after the drivers attach
(`block::partition`): every `0x83` or FAT-typed entry that lies inside the disk
and overlaps no other becomes a `<disk>pN` block device (see
[`block-devices.md`](block-devices.md)). The legacy probe looks at whole disks
only, so a partitioned image still boots as today.

**Docs on the OS volume.** The build embeds the repository's Markdown —
every `*.md` under `docs/` recursively, plus the root `README.md` — into the
ext2 OS volume at `/docs/os/<relative path>` (`build_support/docs_embed.rs`,
`fhs::docs::OS_DOCS`), e.g. `/docs/os/architecture/boot.md` and
`/docs/os/README.md`. The Docs app and the Editor read them through the VFS;
names keep their case (ext2 is case-sensitive) and the tree is kept as written.

**Durability** (`fs::sync_all`, `ext2/state.rs`)

`fs::sync_all()` flushes every mounted filesystem (`Vfs::sync_all`; one failing
mount does not stop the rest). The power path (`process/power.rs`) calls it for
both `shutdown` and `reboot` before the ACPI poke/reset. Native `fsync` (syscall
22) flushes the mount holding its path through the same `Filesystem::flush`.

ext2 keeps no journal unless the volume was given one (`LAZYOS_JOURNAL=1`;
[`journal.md`](journal.md): a journaled volume replays its log at mount and
needs no repair after a crash). Without one, `s_state` says whether the last
stop was clean:

- *dirty first*: the first write of a mount clears the valid bit and flushes
  before anything else is written (if that fails, the change is refused);
- *clean last*: `flush` flushes the device, writes the state saved at mount
  back (valid), and flushes again, so "clean" is never durable ahead of the data;
- a volume mounted unclean (or with the error bit) is logged
  (`ext2: <dev> was not cleanly unmounted`) and stays that way: a clean sync
  restores the mount-time state rather than blessing it. There is no fsck in
  the kernel; the image build is the check. When `cargo build` updates an OS
  volume in place that was not cleanly unmounted, `Ext2::recover`
  (`libs/ext2fs/src/recover.rs`, feature `check`) reclaims its `.unlinked-*`
  orphans and runs the independent checker over the whole volume. If the
  checker finds problems, `Ext2::repair` (`libs/ext2fs/src/repair/`, `no_std`,
  usable by the kernel later) repairs exactly the inconsistencies a crash can
  leave (listed in [`block-cache.md`](block-cache.md#crash-semantics), with
  the repair rules) and the checker runs again. Only a volume it then passes is
  marked clean by the update's closing flush, and the build says what was
  repaired (`re-certified it after repairing 2 leaked blocks (...), 1 link count
  (312: 2->1), ...`). Damage no crash leaves (a reachable block marked free, a
  block claimed twice, a garbled directory, ...) is refused with nothing
  written: that volume, or one with the error bit, stays flagged with a
  `cargo:warning=`, and `LAZYOS_RESET_OS=1` recreates it. Without this one
  unclean stop (a closed QEMU window) would flag the image on every later boot.

Every kernel ext2 mount goes through the write-back block cache
([`block-cache.md`](block-cache.md)): writes stay in memory until a commit,
`flush` writes the cache back before the clean marker, and `fs::flusher`
commits every mount at least every 5 s (the volume stays flagged dirty until a
sync). The loss window after a crash and the inconsistencies an interrupted
writeback can leave are documented there; a failed writeback is reported by
the next `fsync`/`sync` and leaves the error bit in `s_state`.

**Truncate and large files** (`ext2/truncate.rs`, `ext2/indirect.rs`)

`Filesystem::truncate` grows sparsely (no allocation) and shrinks by *detach,
then free*: the inode (or parent table) is rewritten without the pointers before
the blocks go back to the bitmaps, and the bytes about to be cut inside a kept
block are zeroed first. A stop in between leaks blocks (the dirty flag lets the
next recovery reclaim them) but never leaves a block both free and reachable; the kernel
suite sweeps every write of a truncate and an unlink to prove it. The block map
covers direct, single, double and triple indirect blocks with one generic path
walk; files are capped at 2 GiB - 1 (32-bit `i_size`), a write straddling the cap
is short and one past it answers `NoSpace`. Directories still use only direct and
single-indirect blocks.

**Rename** (`libs/ext2fs/src/rename.rs`, issue #407)

A regular file is renamed *new name first*, as Linux's `ext2_rename` does: the
file gains a link, the destination entry is retargeted in place (one block
write; or a new entry is added when there is none), then the source entry and
the extra link go, and only then is the replaced file released. A stop at any
write leaves the file under its old name, its new one, or both (with a link
count of two, so unlinking either is safe), never neither; the worst case is a
leaked link count or an unreferenced victim inode. `confd`'s
write-temp-then-rename store depends on it, and `fs_ext2_confd_store_power_cut_sweep`
cuts the power at every write of a commit to prove it. Directory renames keep
the older remove-then-add order (their link counts carry `..` bookkeeping).

**Working directory** (`process/linux/cwd.rs`, `task/cwd.rs`, issue #365)

Each task has a working directory: an absolute, normalized string in
`Task::cwd` (`None` is `/`, so a task that never `chdir`s allocates nothing;
the string is an `Arc<str>`, so `fork` is a reference-count bump). It is
inherited by `fork`/`vfork` and by threads, kept across `execve` (the task is
the same one) and freed with the task; kernel-started programs begin at `/`.

`cwd::resolve_at(dirfd, path)` is the **one resolver** every path-taking Linux
syscall goes through (`open*`, `stat`/`newfstatat`/`statx`, `chmod`/`chown`/
`utimensat` and friends, `mkdir*`/`rmdir`/`unlink*`/`rename*`, `access`,
`truncate`, `statfs`, `readlink`, `chdir`, `execve`): an absolute path ignores
`dirfd`; a relative one joins onto the cwd (`AT_FDCWD`) or onto the directory
the descriptor was opened on, and the result is folded lexically (`.`/`..`,
`..` clamped at `/`) and bounded by `PATH_MAX` (`ENAMETOOLONG`). The VFS below
only ever sees absolute paths. `cwd::read_path` is the matching reader for the
user string (`EFAULT`/`ENAMETOOLONG`).

- `chdir`/`fchdir`: the target must exist (`ENOENT`), be a directory
  (`ENOTDIR`) and pass the caller's *search* permission (`EACCES`, on the
  target and every ancestor); `fchdir` takes a directory descriptor (`EBADF`
  for a closed one, `ENOTDIR` for anything else). A failure leaves the cwd
  alone.
- `getcwd` returns the byte count including the NUL (the raw syscall's
  contract), `ERANGE` when the buffer is too small, `EFAULT` for a bad buffer.
  `/proc/self/cwd` (`readlink`) reports the same string.
- **A removed cwd.** Nothing pins the directory, so `rmdir` of somebody's cwd
  succeeds (`rmdir(".")` itself is `EINVAL`). Afterwards `getcwd` is `ENOENT`
  and every relative lookup fails with `ENOENT` because the absolute path no
  longer exists; `chdir` to an absolute path recovers. Because the cwd is a
  path rather than an inode, a directory re-created under the same name is the
  cwd again, and `chdir("..")` from a removed directory goes to the lexical
  parent (Linux answers `ENOENT`). `/tmp`, the overlay root and, since
  the ext2 `rmdir` (`libs/ext2fs/src/rmdir.rs`), `/data` can lose a directory.
- Lexical folding means `a/..` never checks that `a` exists or is a directory
  (there are no symlinks, so this differs from POSIX only for that case).

**Two mount tables** (`mod.rs`, `mounts.rs`, issue #136)

Native tasks use the raw table (`FS`); the Linux ABI gets its own table
(`ABI_FS`, reached through the `abi_*` helpers). In the configured layout both
tables mount the same volume instances with the same flags, so there is no
overlay and ABI writes land on ext2. The rest of this section describes the
**legacy layout only** (no `lazyos.cfg`, or its root not found), where the
native table has the boot volume exactly as its backend presents it and the
ABI's `/` is an **overlay**:

| ABI mount | Lower | Upper | Lifetime |
|---|---|---|---|
| `/` | boot volume (FAT, read-only) | private ramfs | kernel heap; discarded on reboot |
| `/tmp` | — | the same ramfs the native table mounts | kernel heap; shared with native |

Reads fall through to the lower layer; the first write, `mkdir`, `rename`, or
`rmdir`/`unlink` copies the node up (recursively for directories) and later
operations use the upper copy. `readdir` unions both layers: upper entries win,
and **whiteouts** hide lower names removed or renamed away. The FAT image is
never written, and native tasks do not see ABI writes; upper-layer inodes carry
the top bit set so the two layers cannot collide in the caches. The upper layer
is capped (`MAX_UPPER_BYTES`, `MAX_UPPER_NODES` in `overlay.rs`); exceeding
either answers `NoSpace`/ENOSPC. The Linux `openat`/`mkdirat`/`unlinkat`/
`renameat` flags (`O_CREAT`, `O_EXCL`, `O_TRUNC`, `O_APPEND`, `O_DIRECTORY`,
`AT_REMOVEDIR`) are honoured in `process/linux/path.rs` and `process/linux/pathops.rs`; `mkdir`(83), `rename`(82),
`unlink`(87), `rmdir`(84) and the `*at` variants are wired to the `abi_*`
surface. Since issue #265 a regular file on *any* mount (the overlay root,
`/tmp`, FAT and ext2 alike) opens as a read-through `OpenFile` descriptor
(below, `fs::abi_read_through`); only fabricated entries (`/proc`, `/etc`
maps, applet aliases off a persistent volume) and directory streams are still
snapshots. A snapshot descriptor is one open file description (`task/snapshot.rs`:
the snapshot, the offset and the path, access mode and `O_APPEND` the open
recorded) that `dup`, `dup2`, `fcntl(F_DUPFD)`, `fork` and `execve` share, so
`prog >/tmp/out 2>&1` writes stdout and stderr at one offset and a child writes
through a descriptor its parent opened. Descriptor writes update the backing
file and patch the description's snapshot, so it reads back its own writes;
unlinking while a descriptor is open keeps the snapshot readable (a later
write through the orphan answers ENOENT).

**Linux descriptors on `/data`** (`openfile.rs`, issue #334)

No mount hands a Linux program a snapshot of a regular file any more (the
overlay root and `/tmp` did until issue #265): the copy would be bounded by the
kernel heap, cost the whole file per open, and a second opener could not see
the first one's writes. An `OpenFile` is a path, a node,
an offset and an access mode. The node (`vfs/node.rs`, docs/performance-plan.md
P5) is the file as its filesystem names it, resolved once at `open`: on ext2 an
inode number and its generation (`ext2fs::FileHandle`; the generation advances
every time the inode is allocated, so a handle on a deleted file never reads the
file that reuses its inode, and answers `ENOENT` instead). `read`, `write`,
`pread64`, `pwrite64` and `fstat` go to the node, with no path walk, permission
walk or mount lookup per call (a write refreshes the table's cached metadata of
the path); `ftruncate` and `fsync` still go to the VFS through the `abi_*`
helpers (see [processes.md](processes.md) for the syscall side), as does every
call on a filesystem without nodes. The program loader opens its image the same
way (`process::image::VfsFile`). Because the VFS names files by path, the
registry of open files keeps each one meaning "the file I opened" (a name
handed to another file behind the registry's back, by the native VFS, makes the
old entry give the name up, so a new open gets the new file):

- `rename` (`abi_rename`) retargets every open file at or under the old path, and
  first parks a file it is about to replace (restoring it if the rename fails);
- `unlink` (`abi_unlink`) of an open file renames it to `<dir>/.unlinked-<n>`
  instead of freeing it, and the last close deletes that entry (all opens of one
  file share one registry entry, so the last one out is known). A stop in
  between leaves the hidden entry behind, the equivalent of an orphan inode.
  The prefix is **reserved**: `open(O_CREAT)`, `mkdir` and `rename` onto a
  `.unlinked-` name answer `EINVAL` (`fs/hidden.rs`), so only the kernel makes
  one and reclaiming by name can never touch a user's file.
- `rmdir` (`abi_rmdir`, issue #612) of a directory whose only entries are such
  parked files succeeds, as on Linux where they have no name: they move to
  `<mount root>/.unlinked-<n>` first (where the reclaim still finds them) and
  move back if the removal is refused. Any other entry, or a reserved name no
  open file owns, still makes the directory `ENOTEMPTY`.
- **Orphan reclaim** (`libs/ext2fs/src/orphans.rs`, issue #346): `mount_data_volume`
  (and `mount_device`) call `Ext2::reclaim_orphans` before the volume is
  visible, and log `fs: /data: reclaimed N orphaned files`. It runs only when
  `s_state` says the volume was not cleanly unmounted, so a clean mount pays
  nothing. The walk is bounded (4096 directories, depth 32), never enters a
  reserved-name directory, and deletes only a regular file (checked from the
  inode mode) whose name has the prefix. Deleting a reserved name runs
  **name last** (free the blocks it reaches, clear the inode, free the inode,
  then remove the entry, each step skipping what an earlier run finished), so a
  stop anywhere, including inside the reclaim, leaves a name that the next
  mount finishes. (Ordinary unlink drops the name first and can strand blocks
  or an inode with no name to find them by.) Free counters can still lag the
  bitmaps after a stop, as for any write; nothing recomputes them at mount. Known
  gap: an orphan on a volume left flagged clean (a `sync` after the unlink, then
  a power cut before another write) is skipped until the next unclean mount;
  it costs space, not consistency. The kernel suite covers this with a cut at
  every write of the last-close delete and of the reclaim
  (`fs_ext2_orphan_*_crash_sweep`) and a random-cut soak.
- open files run as root after `open`, which checked permissions once.

`fsync` flushes only the mount holding the file (`Vfs::flush`); `sync` and the
shutdown path flush every mount (`Vfs::sync_all`). Open, write, close, unlink
cycles are soaked in the kernel suite (`linux_data_soak_*`) and must return every
block, inode, descriptor and registry entry.

**Backends**

| Backend | Status | Notes |
|---|---|---|
| `ramfs` | read/write | `BTreeMap` of nodes, ordered children, owner/mode stamped by VFS |
| `fat` | read-only | FAT12/16, MBR partition, sector reads via the block layer; VFAT long names and nested subdirectories, ASCII case-insensitive; inode = on-disk entry position (root = 1); resolved-path cache |
| `ext2` | read/write | 1/2/4 KiB blocks, group bitmaps, direct + single/double/triple indirect, truncate, clean/dirty state; rejects unknown incompat features and htree directories; optional internal JBD2 journal ([`journal.md`](journal.md)); no symlinks/device nodes |
| `overlay` | read/write (copy-up) | Linux ABI root only; lower is any read-only backend, upper is ramfs |

- ext2 keeps free counters in sync, stamps timestamps from the wall clock
  (`vfs::now()`), and `flush()` flushes the device and marks the
  volume clean (see Durability). Only one block-sized buffer is live per helper and every loop is
  geometry-bounded, so a malformed image cannot hang the kernel.

**Invariants / decisions**

- Kernel helpers (`fs::read`, `fs::abi_*`) stamp the **current task's**
  credentials, so permission checks apply to the native loader as well as
  Linux syscalls.
- FAT stays the boot/recovery format (`/boot`: the kernel and `lazyos.cfg`);
  ext2 is the writable root ([platform-plan.md](../platform-plan.md) section
  4.4).
- Writes need virtio-blk (ATA is read-only in practice); virtio-blk is the
  default QEMU attachment since F1.
- ext2 is **case-sensitive**: a lookup must spell a name exactly as the image
  build stored it, so callers take names from `libs/fhs` (`fhs::bin::BUSYBOX`
  is `/system/bin/busybox`, `fhs::docs::README` is `/docs/os/README.md`)
  instead of relying on the case folding FAT gave. Since F3 nothing folds case:
  the Linux loader maps an applet-shaped name to `/system/bin/<name>` as typed
  (`process/linux/path.rs`), and native `execve` (`process/linux/native.rs`)
  compares names byte for byte, so `TOP` and `top.elf` are not found.

**The OS image (F2).** `cargo build` writes `target/lazyos.img`; `build.rs`
composes it from the bootloader's BIOS part and an ext2 OS volume written by
`libs/ext2fs`, the same code the kernel mounts it with.

| Region | Contents |
|---|---|
| LBA 0 | MBR: entry 1 stage 2 (type 0x20), entry 2 the FAT `/boot` (type 0x0C), entry 3 the OS volume (type 0x83) |
| LBA 1 .. | Stage 2 and the FAT `/boot`: the kernel and a generated `lazyos.cfg` (`root=UUID=<os volume uuid>`, `home=LABEL=lazyhome`) only |
| LBA 131072 (64 MiB) .. | The ext2 OS volume (`LAZYOS_OS_SIZE`, default `512M`, minimum `128M`; the image is that offset plus the size) |

The geometry is fixed so an update never moves data; the build fails when the
FAT partition ends past LBA 131072. Everything except the kernel and
`lazyos.cfg` goes onto the OS volume through the `Sink` trait in
`build_support/os_image.rs`: each embed module (`drivers`, `docs_embed`,
`rhai_embed`, `lazyrad_embed`, `xui_embed`, and `build.rs` itself) keeps its
selection logic and only its sink changed, to an OS file list
(`OsFile { path, source, mode }`; the mode follows the directory: 0755 under
`/system/bin`, 0644 everywhere else, all root-owned).
`build_support/os_layout.rs` is the declarative directory table, and an update
applies each directory's mode and owner again so an older image converges to it:

| Path | Mode, owner | What |
|---|---|---|
| `/boot`, `/home`, `/transient` | 0755 root | mount points |
| `/system` (`bin`, `etc`, `share`, `packages`) | 0755 root | the build's files; `packages` empty until F5 |
| `/conf` | 0700 root | `confd`'s store (`fhs::state::CONF_ROOT`); only `confd` reads it |
| `/conf/svc` | 0700 root | non-key/value service state, `<service>/` each, made by its owner |
| `/logs` | 0750 root | `logd`'s journals (`libs/logstore`) and `pkgd`'s `pkg.log`: everyone's activity, so not world-readable |
| `/apps`, `/docs/apps` | 0755 root | installed apps and their documentation, written only by `pkgd` |
| `/home/<name>` | 0700, the account's uid:gid | one per account of the embedded `/system/etc/passwd` whose home is `/home/<name>` (`fhs::home_of`); a mounted home volume hides them |
| `/data` | 0755 root | transitional: nothing new is written there (`lazyrad` writes the user's home since F4); F7 removes it |

All those services run as uid 0 today; when #446/#447 give each its own uid,
the owners follow. F4 stopped seeding `/data/home/<user>` and `/data/tmp`: an
update removes them only when empty, so a user's files there survive until F7
(`build_support/tests/f4_layout_tests.rs`). Since F3
every file sits below one of these directories: programs in `/system/bin`
(`fhs::bin`), the core packages in `/system/packages` (F5: the desktop apps,
which `pkgd` installs into `/apps`), `passwd` in `/system/etc`, `mime.types`, the
samples (`/system/share/samples`) and the lazyrad projects
(`/system/share/lazyrad`) in `/system/share`, the documentation in
`/docs/os`. The root holds directories only; an update of an F2 image deletes
the old flat names because the old manifest lists them and the new one does
not (`build_support/tests/f3_layout_tests.rs`).

*Create versus update.* `/system/.image-manifest` (one `d <path>` or `f <path>`
line per directory and file the build placed, sorted) is the only record of what
an update may replace or delete. `build_support/os_image.rs::plan` decides:

1. **Create** when the image is missing, `LAZYOS_RESET_OS=1` is set, or it fails
   validation: MBR signature, entry 3 of type 0x83 starting at LBA 131072, a
   file length equal to the offset plus the entry's size, the ext2 magic (the
   volume opens through `libs/ext2fs`), and a readable, well-formed manifest. A
   failure prints a `cargo:warning=` with the reason, never silently. A new
   random UUID is generated, the volume formatted and populated, and the result
   written to `target/lazyos.img.tmp`, then renamed over the image.
2. **Update** otherwise, keeping the UUID: rewrite LBA 0..131072 (the MBR with
   all three entries, stage 2, the FAT with a `lazyos.cfg` carrying that UUID),
   open the volume, delete the old-manifest paths the new list lacks (files are
   unlinked, directories removed only when empty), write every file of the new
   list (truncate + write), write the manifest last, flush. A path in neither
   manifest is never touched, which is what preserves `/apps`, `/conf`, `/logs`,
   `/docs/apps`, `/home`, `/data` contents and anything a user created.
3. A different `LAZYOS_OS_SIZE` on a valid image is an error that says to set
   `LAZYOS_RESET_OS=1` (no resize). An update opens the file exclusively and
   fails with a "stop QEMU and retry" message when something holds it.

CI is hermetic: every workflow that builds an image sets `LAZYOS_RESET_OS=1`
(`image.yml` also builds without it to check the in-place update). Host tests:
`cargo test -p build-support-tests` (layout table, manifest diff with a user
file outside the manifest surviving, the 64 MiB guard, validation failures
leading to create, locked image, size change) plus an ignored test that checks a
real image with the independent checker in `libs/ext2fs`
(`LAZYOS_CHECK_IMAGE=target/lazyos.img cargo test -p build-support-tests --
--ignored`); `tools/ci/check_os_image.sh` runs `e2fsck -fn` and `debugfs` over a
fresh and an updated image. `cargo run -q -p ext2fs --example osread -- IMAGE
cat|stat|ls PATH` reads the OS volume of an image from the host, read-only (the
shutdown harness reads `/logs` with it, which is root-only in the guest).

**The home volume (host tooling).** `target/home.img` is a persistent ext2 image
(label `lazyhome`, found by `home=LABEL=lazyhome`) with one `<user>/` directory
per demo account at its root (0700, the account's uid:gid) and no `/home` or
`/tmp`. `tools/mkdisk/` formats
it in pure Python (Windows has no `mkfs.ext2`): `python -m tools.mkdisk PATH
--home-volume`. `tools/run_demo.py` creates it on first use and attaches it as a
second `virtio-blk-pci` device; flags are `--home-disk PATH`, `--no-home-disk`
and `--reset-home` (confirmation prompt unless `--yes`), and `--reset-os` sets
`LAZYOS_RESET_OS=1` for the build. `qemu_shot.py`/`qemu_session.py` accept
`--home-disk PATH` (off by default so CI stays hermetic), and the launcher GUI
has a "Home volume" group. Without a home disk `/home` is a directory on `/`.
The legacy `--data-disk` still attaches a second ext2 volume (mounted only in the
legacy layout; F7 migrates it).

*Persistence rule.* The OS volume and the home volume survive runs and
rebuilds: a rebuild updates the OS volume in place and launchers create
`home.img` only when it is missing. Reset (`LAZYOS_RESET_OS=1` or
`--reset-os`; `--reset-home`) is explicit, erases everything and rewrites the
seeded layout.

**Status.** Working: the build-made image (FAT `/boot` + ext2 OS volume,
created or updated in place), ramfs `/transient` and `/tmp`, ext2 read/write,
permissions, caches, `umask`, `chmod`/`chown`/`utimensat` on every writable
backend, the Linux ABI sharing the same mounts, an ext2 root with truncate and
large files, synced on shutdown, and Linux descriptors that read and write it in
place (`pread64`/`pwrite64`/`ftruncate`/`fsync`/`sync`/`statfs`). The in-kernel
suite (`fs_ext2_*` over a `FakeDisk`, plus the `libs/ext2fs`-built root mounted
from a `lazyos.cfg`) holds the correctness, crash-ordering and soak coverage.
F4 (issue #508) put the services on the tree: `confd` in `/conf` (seeded once
from `/data/confd`), `logd` journals and `pkg.log` in `/logs`, `pkgd` in `/apps`
and `/docs/apps`, `lazyrad` in the user's home (`$HOME/projects`,
`$HOME/.apps/lazyrad`, an installed app's `$HOME/.apps/<system_name>`). Open:
symlinks, cross-mount rename, page cache, resizing an existing OS image, the
rest of F4 (the `admin`/`user` accounts, `$HOME`) and F5 to F7 (packages, the ABI overlay, migrating and
removing `/data`).
