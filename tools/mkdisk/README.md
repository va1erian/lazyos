# mkdisk: the persistent data volume

Pure-Python ext2 formatter for `target/data.img`, the volume the kernel mounts
read/write at `/data` (Windows has no `mkfs.ext2`). Architecture:
[`docs/architecture/filesystem.md`](../../docs/architecture/filesystem.md).

```bash
python -m tools.mkdisk [PATH] [--size 64M] [--label NAME] [--block-size N] [--force]
                       [--root-mode 755] [--root-uid 0] [--root-gid 0] [--no-seed]
```

## Seeded layout and ownership

The guest has no `chown` yet, so ownership must be right when the volume is
formatted. By default the volume gets:

| Path | Mode | Owner | Why |
|------|------|-------|-----|
| `/data` (root) | `0755` | `root:root` | Safe default; nothing but root can add top-level entries |
| `/data/home` | `0755` | `root:root` | Parent of the user homes |
| `/data/home/<user>` | `0755` | that account's `uid:gid` | One per demo account whose home is `/home/<user>` (today `alice`) |
| `/data/tmp` | `1777` | `root:root` | World-writable with the sticky bit, so users cannot delete each other's files |

`--root-mode/--root-uid/--root-gid` change the `/data` root itself (for example
`--root-mode 1777` for a shared drop box). `--no-seed` writes only the root and
`lost+found`.

The accounts are **not** copied: `accounts.py` parses the built-in passwd table
out of `user/src/bin/accountsd.rs`, and `test_seed.py` fails if that table and
the `PASSWD` file `build.rs` puts on the boot volume ever differ. ids are limited
to 16 bits because that is all the kernel's ext2 driver stores.

## Persistence rule

The volume survives runs. `run_demo.py` and the launcher create it only when it
is missing and never regenerate it implicitly. Reset is explicit and confirmed:
`python tools/run_demo.py --reset-data` (asks first, `--yes` to skip) or the
launcher's **Reset volume** button, which lists the directories it will create.
A reset erases everything and rewrites the seeded layout.

## Tests

```bash
python tools/mkdisk/test_mkdisk.py   # geometry, driver-mount rules, mini fsck
python tools/mkdisk/test_seed.py     # ownership, sticky bit, free counts, CLI, account drift
python tools/lazygui/test_catalog.py # launcher plan flags and Reset
```

CI (`.github/workflows/mkdisk.yml`) also runs `e2fsck -fn` over the default
seeded image and several block sizes, root modes and multi-group sizes.

## Files

| File | Role |
|------|------|
| `geometry.py` | Block, group and inode placement from the requested size |
| `layout.py` | `Layout` / `DirSpec`: root attributes and the seeded directories |
| `accounts.py` | Reads the demo accounts from `accountsd.rs` |
| `tree.py` | Directory inodes and blocks for the root and the seeded tree |
| `ext2.py` | Superblock, descriptors, bitmaps, `lost+found`; assembles the extents |
| `volume.py` | `format_image`, `ensure_volume`, `status` used by the launchers |
