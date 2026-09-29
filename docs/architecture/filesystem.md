# Filesystem: VFS, ramfs, FAT, ext2

**What it is.** The VFS core (mounts, path resolution, permissions, caches) and
its three backends: a read-write ext2 driver, a read-only FAT12/16 driver, and
an in-memory ramfs mounted at `/tmp`.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/fs/mod.rs` (+ `abi_attr.rs`) | Init/mount, kernel-side `read`/`abi_*` entry points (`abi_setattr*` in `abi_attr.rs`) |
| `kernel/src/fs/vfs.rs` (+ `vfs/{filesystem,meta,path,cache}.rs`) | `Vfs`, resolution; `Filesystem` trait, `Path`, `Id`, permissions and the dentry/inode caches in the submodules |
| `kernel/src/fs/vfs/{attr,setattr}.rs` | Timestamps, the filesystem clock, `SetAttr`; the `chmod`/`chown`/`utimensat` rules (issue #345) |
| `kernel/src/fs/ramfs.rs` (+ `ramfs/{node,capacity}.rs`) | In-memory tree; root inode 1 (issue #98); a node and its attributes in `node.rs` |
| `kernel/src/fs/fat.rs` | Read-only FAT12/16 on the boot volume |
| `kernel/src/fs/ext2.rs` (+ `ext2/{layout,blocks,indirect,truncate,state,dir,attr,fsimpl}.rs`) | Read/write ext2 rev 0/1 (issues #99, #333, #345) |
| `kernel/src/fs/overlay.rs` | Copy-up overlay for the Linux ABI root (issue #136) |
| `kernel/src/fs/openfile.rs` | Open files on `/data` for Linux descriptors: in-place I/O, follow renames, unlink-while-open (issue #334) |
| `kernel/src/fs/vfs/mountops.rs` | Whole-mount operations: `flush`, `sync_all`, `statfs`, `mount_point` |

**VFS semantics** (`vfs.rs`, issue #98)

- The resolver is **symlink-free**: a path is just directory entries. `Path`
  folds `.`/`..` lexically and clamps `..` at the root; relative paths resolve
  from `/` (no per-task cwd yet; `chdir` is a no-op).
- Mounts are `(point, Filesystem)`; resolution picks the **longest** mount-point
  prefix, so `/tmp/notes` lands in ramfs while `/tmp2` stays on the root volume.
- Every entry point takes an `Id { uid, gid }` from
  `ipc::credentials` (`Id::current`); ancestors need `EXECUTE` (search) and the
  final node the operation's `READ`/`WRITE` mask. Root (uid 0) bypasses bits
  ([security-model.md](../security-model.md) section 4.1). The sticky bit
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
`time_t`). Every backend stamps from one clock, `vfs::now()` (PIT uptime until
an RTC driver lands), so a write and a `touch` agree; reads never move `atime`
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

**Durability** (`fs::sync_all`, `ext2/state.rs`)

`fs::sync_all()` flushes every mounted filesystem (`Vfs::sync_all`; one failing
mount does not stop the rest). The power path (`process/power.rs`) calls it for
both `shutdown` and `reboot` before the ACPI poke/reset. Native `fsync` (syscall
22) flushes the mount holding its path through the same `Filesystem::flush`.

ext2 keeps no journal, so `s_state` says whether the last stop was clean:

- *dirty first*: the first write of a mount clears the valid bit and flushes
  before anything else is written (if that fails, the change is refused);
- *clean last*: `flush` flushes the device, writes the state saved at mount
  back (valid), and flushes again, so "clean" is never durable ahead of the data;
- a volume mounted unclean (or with the error bit) is logged
  (`ext2: <dev> was not cleanly unmounted`) and stays that way: a clean sync
  restores the mount-time state rather than blessing it. There is no fsck here.

**Truncate and large files** (`ext2/truncate.rs`, `ext2/indirect.rs`)

`Filesystem::truncate` grows sparsely (no allocation) and shrinks by *detach,
then free*: the inode (or parent table) is rewritten without the pointers before
the blocks go back to the bitmaps, and the bytes about to be cut inside a kept
block are zeroed first. A stop in between leaks blocks (the dirty flag lets an
fsck reclaim them) but never leaves a block both free and reachable; the kernel
suite sweeps every write of a truncate and an unlink to prove it. The block map
covers direct, single, double and triple indirect blocks with one generic path
walk; files are capped at 2 GiB - 1 (32-bit `i_size`), a write straddling the cap
is short and one past it answers `NoSpace`. Directories still use only direct and
single-indirect blocks.

**Two mount tables** (`mod.rs`, issue #136)

Native tasks use the raw table (`FS`): the boot volume exactly as its backend
presents it. The Linux ABI gets its own table (`ABI_FS`, reached through the
`abi_*` helpers) where `/` is an **overlay**:

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
surface. Descriptor writes update the backing file and patch the fd's snapshot,
so a descriptor reads back its own writes; unlinking while a descriptor is open
keeps the snapshot readable (a later write through the orphan answers ENOENT).

**Linux descriptors on `/data`** (`openfile.rs`, issue #334)

The overlay root and `/tmp` hand a Linux program a snapshot of the file; `/data`
does not, because the copy would be bounded by the kernel heap and a second
opener could not see the first one's writes. An `OpenFile` is a path, an offset
and an access mode; `read`/`write`/`pread64`/`pwrite64`/`ftruncate`/`fsync` go to
the VFS at that offset through the `abi_*` helpers (see
[processes.md](processes.md) for the syscall side). Because the VFS names files
by path, the registry of open files keeps each one meaning "the file I opened":

- `rename` (`abi_rename`) retargets every open file at or under the old path, and
  first parks a file it is about to replace (restoring it if the rename fails);
- `unlink` (`abi_unlink`) of an open file renames it to `<dir>/.unlinked-<n>`
  instead of freeing it, and the last close deletes that entry (all opens of one
  file share one registry entry, so the last one out is known). A stop in
  between leaves the hidden entry behind, the equivalent of an orphan inode.
  The prefix is **reserved**: `open(O_CREAT)`, `mkdir` and `rename` onto a
  `.unlinked-` name answer `EINVAL` (`fs/hidden.rs`), so only the kernel makes
  one and reclaiming by name can never touch a user's file.
- **Orphan reclaim** (`fs/ext2/orphans.rs`, issue #346): `mount_data_volume`
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
| `fat` | read-only | FAT12/16, MBR partition, sector reads via the block layer; 8.3 short names only |
| `ext2` | read/write | 1/2/4 KiB blocks, group bitmaps, direct + single/double/triple indirect, truncate, clean/dirty state; rejects unknown incompat features and htree directories; no journal/symlinks/device nodes |
| `overlay` | read/write (copy-up) | Linux ABI root only; lower is any read-only backend, upper is ramfs |

- ext2 keeps free counters in sync, stamps timestamps from PIT ticks (best
  effort until an RTC driver), and `flush()` flushes the device and marks the
  volume clean (see Durability). Only one block-sized buffer is live per helper and every loop is
  geometry-bounded, so a malformed image cannot hang the kernel.

**Invariants / decisions**

- Kernel helpers (`fs::read`, `fs::abi_*`) stamp the **current task's**
  credentials, so permission checks apply to the native loader as well as
  Linux syscalls.
- FAT stays the shipped boot/recovery format; ext2 is the writable volume
  ([platform-plan.md](../platform-plan.md) section 4.4).
- ATA is read-only in practice, so ext2 write traffic needs virtio-blk; the
  default QEMU image uses ATA + FAT.

**The data volume (host tooling).** `target/data.img` is a persistent 64 MiB
ext2 image (4 KiB blocks, revision 1, `lost+found`, sparse-super backups) that
survives across QEMU runs. `tools/mkdisk/` formats it in pure Python (Windows
has no `mkfs.ext2`): `python -m tools.mkdisk [PATH] [--size 64M] [--label NAME]
[--block-size N] [--root-mode M] [--root-uid U] [--root-gid G] [--no-seed]
[--force]`. Its layout is checked against `Ext2::open`'s validation and by a
miniature fsck in `tools/mkdisk/test_mkdisk.py`/`test_seed.py`; CI also runs
`e2fsck -fn` over it. `tools/run_demo.py` creates it on first use and attaches it
as a **second** `virtio-blk-pci` device (`-drive
format=raw,file=target/data.img,if=none,id=data`); flags are `--data-disk
PATH`, `--no-data-disk`, and `--reset-data` (confirmation prompt unless
`--yes`). The launcher GUI has a matching "Data volume" group (path/size/
existence, attach toggle, Reset button that lists what it will create), and
`qemu_shot.py`/`qemu_session.py` accept `--data-disk PATH` (off by default so CI
stays hermetic).

*Seeded layout and ownership.* There is no `chown` yet, so ownership is set at
format time. The `/data` root stays `root:root 0755` (non-root users cannot add
top-level entries); the formatter also creates `/data/home` (`root 0755`), one
`/data/home/<user>` (`0755`, owned by that account's uid/gid) for every demo
account homed under `/home`, and `/data/tmp` (`1777`, sticky). The accounts are
read from `accountsd`'s built-in passwd table (`user/src/bin/accountsd.rs`) by
`tools/mkdisk/accounts.py`, and a test fails if that table drifts from the
`PASSWD` file `build.rs` embeds. `--root-mode/--root-uid/--root-gid` adjust the
root itself; `--no-seed` leaves only `lost+found`. Ids are limited to 16 bits,
the width the driver stores (`check_owner`). Once VFS attributes (`chown`) land
this layout can shrink to a bare root.

*Persistence rule.* The volume survives runs: launchers create it only when it
is missing and never regenerate it implicitly. Reset (`--reset-data`, or the
GUI's Reset button) is explicit and confirmed, erases everything and rewrites
the seeded layout.

**Status.** Working: FAT boot, ramfs `/tmp`, ext2 read/write, permissions,
caches, `umask`, `chmod`/`chown`/`utimensat` on every writable backend, the
Linux ABI copy-up overlay (`O_CREAT`/`mkdir`/`rename`/
`unlink`/`rmdir`, fd writes), an ext2 `/data` volume with truncate and large
files, synced on shutdown, and Linux descriptors that read and write it in place
(`pread64`/`pwrite64`/`ftruncate`/`fsync`/`sync`/`statfs`). The in-kernel suite
(`fs_ext2_*` over a `FakeDisk`) holds the correctness, crash-ordering and soak
coverage; a session has `/data` when a second block device carries ext2, which
`tools/run_demo.py` attaches by default (`target/data.img`, see the data volume
section above). Open: symlinks, cross-mount rename, per-process
cwd, page cache, and overlay persistence to the writable volume.
