# Filesystem plan: an ext2 OS volume with a real file tree

Replaces the flat FAT12/16 boot volume (8.3 names, everything at the root) and
the optional ext2 `/data` disk with an ext2 OS volume carrying a real tree,
long case-sensitive names and persistent storage. Companion to
[`architecture/filesystem.md`](architecture/filesystem.md) (current state),
[`architecture/block-devices.md`](architecture/block-devices.md) and
[`packages.md`](packages.md).

**Status (2026-10-03):** F0-F5 done (#478, #506, #525). F6 holds in the
configured layout: the Linux ABI mounts the same volumes as native tasks, with
no overlay, and `/tmp` is the `/transient` ramfs (`configured` in
`kernel/src/fs/mounts.rs`); the copy-up overlay and `/data` remain only in the
`legacy` layout, the fallback when there is no `lazyos.cfg` or its root
volume is not found. F7 is not started.

## 1. Where it started

History: the layout before F0. F2 replaced the FAT root with the ext2 OS
volume, and F3 moved every program to `/system/bin` under its real name
(`/system/bin/init`, `/system/bin/busybox`, ...), `PASSWD` to
`/system/etc/passwd`, `MIME.TYP` to `/system/share/mime.types`, the samples to
`/system/share/samples` and the docs to `/docs/os`; see
[`architecture/filesystem.md`](architecture/filesystem.md) for the current
state.

- `bootloader 0.11` `DiskImageBuilder` makes an MBR with one FAT partition that
  holds the kernel and ~60 flat files (`SUPER.ELF`, `XTERM.ELF`, `PASSWD`,
  `XAPPS.LST`, `MIME.TYP`, ...); the only directories are `docs/` and `LAZYRAD/`.
- `fs::init` mounts the first FAT volume at `/`, ramfs at `/tmp`, and the first
  ext2 volume on another disk at `/data`. No partition table parsing, no mount
  table or flags, and `readdir` omits mount points (xui apps inject
  `ROOT_MOUNTS`).
- Programs are spawned by uppercase 8.3 name (init `MANIFEST`,
  `process/linux/native.rs`, `root_elf_path`); the spawn command line is split
  on spaces; task-name interning assumes a case-insensitive filesystem.
- State lives under `/data` (`/data/confd`, `/data/apps`, `/data/log`, lazyrad's
  `/data/...`); `logd` is memory-only; `accountsd` reads `PASSWD` from FAT.
- No app is lzp-installed: xui apps are FAT `X*.ELF` files listed in `XAPPS.LST`.
- The Linux ABI root is a copy-up overlay (FAT lower, ramfs upper): nothing
  persists. (Today only the `legacy` layout in `kernel/src/fs/mounts.rs` still
  builds this overlay.)

## 2. Target tree

The OS is **one unit**: one ext2 volume that apps are installed over. Home
directories are a **separate volume**.

| Path | Backing | Flags | Contents / owner |
|---|---|---|---|
| `/` | OS volume (ext2, MBR entry 3 of the boot disk) | rw | the OS |
| `/boot` | FAT, MBR entry 2 of the boot disk (entry 1 is the bootloader's stage 2) | ro at runtime | bootloader, kernel, `lazyos.cfg` |
| `/system` | directory on `/` | root-owned, written only by updates | `bin/` (core binaries and services), `share/` (fonts, icons, `mime.types`), `etc/` (`passwd`), `packages/*.lzp` (core apps) |
| `/apps` | directory on `/` | written only by `pkgd` | every app, core apps included: `/apps/<system_name>/<version>-<digest8>/` |
| `/conf` | directory on `/` | root/confd only | confd store; `/conf/svc/<name>/` for non key-value service state |
| `/docs` | directory on `/` | | `os/` from the build, `apps/<system_name>/` from `pkgd` |
| `/logs` | directory on `/` | logd, pkgd | persistent journals, `pkg.log` |
| `/home` | home volume (ext2, its own virtio disk) | rw, nosuid | `/home/<user>`; per-app data in `/home/<user>/.apps/<system_name>/` |
| `/transient` | ramfs | 1777 | temporary files; `/transient/run` for runtime state |

- The kernel mounts `/`, `/home` and `/boot` by **label/UUID** from
  `/boot/lazyos.cfg`, never by device order.
- Without a home volume, `/home` is a plain directory on `/`; the system still
  works.
- The Linux ABI sees the same tree plus synthetic `/dev`, `/proc`, `/etc` and
  `/bin` (mapped to `/system/bin`); its `/tmp` is the `/transient` ramfs.
- `/data` and every 8.3 name disappear.

## 3. Decisions

| Topic | Decision |
|---|---|
| Storage driver | **virtio-blk** is the only writable disk, everywhere (demo, screenshot tools, kernel tests, CI). ATA stays a read-only fallback; an OS volume found on ATA mounts read-only with a log line. AHCI/NVMe are a later real-hardware track. |
| Accounts | Two hardcoded accounts, no account management: `admin` (uid 0, home `/home/admin`) and `user` (uid 1000, home `/home/user`). They replace `root` and `alice`. Defaults in `/system/etc/passwd`; `CreateUser` stays out of scope. |
| Core apps | Shipped as lzp in `/system/packages`, installed by `pkgd` into `/apps` with `origin = core`. They **cannot be removed** (`pkgd` refuses and audits; the installer shows no Remove button) but can be **hidden from the menu**: per-user confd key `user/menu/hidden/<system_name>`, machine default under `sys/menu/hidden/`. Hiding affects only the menu: MIME "open with" and launch-by-name still work. A user may install a newer version of a core app over it. |
| Rebuilds | The image is created on first build or `--reset-os`; later builds **update it offline** (rewrite `/boot`, `/system`, `/docs/os`), keeping `/apps`, `/conf`, `/logs` and `/docs/apps`. `pkgd` upgrades core packages at the next boot. `home.img` is created once and erased only by `--reset-home`. CI always uses a fresh image and no home disk. |
| Deferred to the next iteration | Real account management, crash safety in the kernel (journal or boot-time fsck; the image build already repairs what a crash leaves, offline, see section 5), symlinks, quotas, AHCI/NVMe. |

## 4. Phases

Every kernel phase ships correctness **and** soak tests in `kernel/src/tests/`
and passes `python tools/test/run.py --accel none` (see `AGENTS.md`).

### F0: paths in one place

- `libs/fhs`: a `no_std` crate with every well-known path (`SYSTEM_BIN`,
  `APPS_ROOT`, `CONF_ROOT`, `LOGS_ROOT`, `DOCS_OS`, `HOME_ROOT`, `TRANSIENT`, ...).
  Every literal path in `kernel/`, `user/`, `libs/`, `xui-app/`, `lazyrad-os/`
  moves to it, so later phases change constants rather than strings.

### F1: kernel mounts on virtio-blk

- MBR parsing; one block device per partition (`virtio0p2`, `virtio0p3`).
- A mount table with flags (`ro`, `noexec`, `nosuid`), filled from
  `/boot/lazyos.cfg` (root, home and boot UUIDs).
- `readdir` lists mount points; drop `ROOT_MOUNTS` from the xui apps.
- Recovery boot: no OS volume found means FAT `/boot` plus ramfs only, logged.
- virtio-blk throughput, since the whole OS now loads through it:
  requests of up to 64 KiB as chained per-page descriptors over a larger
  bounce region. DMA into caller buffers and several requests in flight come
  later.
- Every launcher and CI job boots from virtio-blk.

### F2: one ext2 implementation, image built by the build

- Extract `kernel/src/fs/ext2/*` into `libs/ext2fs` (`no_std` + `alloc`,
  generic over a block trait), used by the kernel and by `build.rs` on the host.
- `build.rs` writes `target/lazyos.img`: MBR, FAT `/boot`, ext2 `/` populated
  with files, modes and owners. Offline update of an existing image (section 3).
- `tools/mkdisk` creates `home.img` (`/home/admin`, `/home/user`) and stays the
  independent checker next to `e2fsck -fn` in CI.

### F3: real names and spawning

- Lowercase binaries in `/system/bin`; init's `MANIFEST`, `native.rs` and
  `root_elf_path` become lookups there (`busybox`, `rhai` included).
- An argv-vector spawn syscall (paths may contain spaces).
- Native spawn enforces the exec bit; spawn interning becomes case-sensitive.

### F4: services on the tree

- **confd** stores in `/conf`; `/transient/conf` remains the degraded fallback
  (done: `/data/confd` is merged in once, then `/conf/.seeded-from-data`;
  `/conf/svc/<service>/` is documented for non-key/value state).
- **logd** writes persistent journals to `/logs/<service>.log` with size caps
  and rotation (done: `libs/logstore`, 256 KiB per file, `.1`/`.2`, an 8 MiB
  budget excluding `pkg.log`; `Sources`/`TailFile` for uid 0).
- **accountsd** reads `/system/etc/passwd` (`admin`, `user`); rename `root` and
  `alice` across code, tests and tools.
- **pkgd** installs to `/apps`, logs to `/logs/pkg.log`, writes package docs to
  `/docs/apps/<system_name>/`; its install-source rule ("single-component path
  = boot volume") is replaced by `/transient` and the caller's home (done:
  `pkgstore::tree` shared with the host and kernel soaks, a writability probe,
  `pkgd` in the lifecycle contract).
- **image layout** (done): `/conf` and `/conf/svc` 0700, `/logs` 0750,
  `/apps` and `/docs/apps` 0755, a 0700 `/home/<name>` per passwd account;
  `/data/home` and `/data/tmp` are no longer seeded.
- **lazyrad** moves `/data/...` to `/home/<user>/...`.
- **mimed** reads `/system/share/mime.types` (done in F3; the services evidence
  fails on `MIME:GUESS:INFO no override file`).

### F5: every app is an lzp

- The build packages each xui app (`tools/pkg/build.py`) into
  `/system/packages/`.
- `pkgd` provisions on first boot and after each update: install a missing core
  package, upgrade an older one, record `origin = core`.
- Core apps are non-removable and hideable (section 3); Settings gets a
  "Hidden apps" list.
- Manifest additions: `autostart`, menu `category`, `$HOME`-relative `files`
  permissions (replacing `/data/home/*/...`).
- Delete `XAPPS.LST` and the `X*.ELF` names.

### F6: Linux ABI on the real tree

- Drop the overlay; the ABI table is the real tree plus the synthetic
  `/dev`, `/proc`, `/etc`, `/bin`; `/tmp` is `/transient`. Writes persist under
  normal permissions.

### F7: migration and cleanup

- One-shot migration of an existing `data.img`: `/data/home` to `home.img`,
  `/data/confd` to `/conf`, `/data/apps` to `/apps`, `/data/log` to `/logs`.
  Home directories map by account, not by name:
  - `/data/home/alice` (uid/gid 1000) becomes `user/` on the home volume.
    `user` keeps uid/gid 1000, so files keep their owner as stored, and
    nothing is re-chowned.
  - Any other `/data/home/<name>` (no such account exists after the rename)
    goes to `admin/migrated/<name>/`, owners unchanged.
  - The seeded `data.img` has no `/data/home/root`; do not assume one exists.
  - Collisions never overwrite: if the target already has an entry of that
    name, the source entry lands in `<target>/migrated-<unix time>/` instead
    and the tool reports it.
  - The source volume is only read, so a failed migration can be re-run.
- Remove `/data`, the short-name code paths, and `--data-disk`; FAT shrinks to
  a read-only driver for `/boot`.
- Update `architecture/filesystem.md`, `packages.md`, `AGENTS.md` and the tools'
  READMEs.

## 5. Next iteration

Account management (`CreateUser`, password changes, persistent account
database), crash safety (boot-time consistency check, then a journal), ext2
symlinks, quotas on `/home` and `/logs`, and AHCI/NVMe for real hardware.

**Block cache: done** ([`architecture/block-cache.md`](architecture/block-cache.md)).
A write-back cache inside `libs/ext2fs`, used by every kernel ext2 mount and
the host image build: frames as pages, writeback in a crash-safe phase order
(fresh blocks, bitmaps, inode tables, other content, superblock) coalesced into
64 KiB virtio requests, frees deferred to the commit, barriers where an
operation needs an order the phases cannot give (renames, orphan deletes,
unaligned truncates), and a periodic flusher bounding the loss window to about
5 s. First-boot provisioning went from 139.6 s to about 4 s under WHPX. A crash
between syncs now leaves a wider (documented) set of repairable
inconsistencies, still flagged by `s_state`. The kernel has no fsck or journal
(deferred); the image build repairs that damage offline when it next updates
the volume (`Ext2::recover` and `Ext2::repair`, #512 and #525; see
[`architecture/filesystem.md`](architecture/filesystem.md)). Next for the block layer: DMA into the cache's frames (no bounce
copy) and several requests in flight.
