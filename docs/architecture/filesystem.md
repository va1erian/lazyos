# Filesystem: VFS, ramfs, FAT, ext2

**What it is.** The VFS core (mounts, path resolution, permissions, caches) and
its three backends: a read-write ext2 driver, a read-only FAT12/16 driver, and
an in-memory ramfs mounted at `/tmp`.

**Key files**

| Path | Role |
|---|---|
| `kernel/src/fs/mod.rs` | Init/mount, kernel-side `read`/`stat`/`vfs_*` entry points |
| `kernel/src/fs/vfs.rs` | `Filesystem` trait, `Vfs`, `Path`, `Id`, permissions, caches |
| `kernel/src/fs/ramfs.rs` | In-memory tree; root inode 1 (issue #98) |
| `kernel/src/fs/fat.rs` | Read-only FAT12/16 on the boot volume |
| `kernel/src/fs/ext2.rs` | Read/write ext2 rev 0/1 (issue #99) |

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
- `Filesystem` trait: `name`, `lookup`/`stat`, `read`, `write`, `create`,
  `mkdir`, `unlink`, `rename`, `readdir`. The VFS checks permissions before
  calling in, so a backend only enforces what is intrinsic (e.g. FAT returns
  `ReadOnly`). `FsError` variants map to errno in the Linux layer.

**Mounting** (`mod.rs`)

1. `block::init()` probes devices; each device is tried as FAT first (only the
   boot device), then as ext2 (`Ext2::open(device)`).
2. The first volume becomes `/`; `mounted` reports whether a volume was found.
3. A fresh `RamFs` is always mounted at `/tmp`, so the VFS is usable even with
   no disk volume.
4. `mount_device(point, device)` is the named-device mount surface.

**Backends**

| Backend | Status | Notes |
|---|---|---|
| `ramfs` | read/write | `BTreeMap` of nodes, ordered children, owner/mode stamped by VFS |
| `fat` | read-only | FAT12/16, MBR partition, sector reads via the block layer; 8.3 short names only |
| `ext2` | read/write | 1/2/4 KiB blocks, group bitmaps, direct + single indirect; rejects unknown incompat features and htree directories; no journal/symlinks/device nodes |

- ext2 keeps free counters in sync, stamps timestamps from PIT ticks (best
  effort until an RTC driver), and `flush()` writes the superblock and flushes
  the device. Only one block-sized buffer is live per helper and every loop is
  geometry-bounded, so a malformed image cannot hang the kernel.

**Invariants / decisions**

- Kernel helpers (`fs::read`, `fs::stat`, `fs::vfs_*`) stamp the **current
  task's** credentials, so permission checks apply to the native loader as well
  as Linux syscalls.
- FAT stays the shipped boot/recovery format; ext2 is the writable volume
  ([platform-plan.md](../platform-plan.md) section 4.4).
- ATA is read-only in practice, so ext2 write traffic needs virtio-blk; the
  default QEMU image uses ATA + FAT.

**Status.** Working: FAT boot, ramfs `/tmp`, ext2 read/write, permissions,
caches, `umask`. Open: symlinks, `rmdir`, cross-mount rename, per-process cwd,
page cache, and a write test path in CI.
