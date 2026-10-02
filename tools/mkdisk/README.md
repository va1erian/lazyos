# mkdisk: the persistent data and home volumes

Pure-Python ext2 formatter for `target/home.img` (the `/home` volume) and the
legacy `target/data.img` (mounted read/write at `/data`, now opt-in with
`--data-disk`; Windows has no `mkfs.ext2`). Architecture:
[`docs/architecture/filesystem.md`](../../docs/architecture/filesystem.md).

```bash
python -m tools.mkdisk [PATH] [--size 64M] [--label NAME] [--block-size N] [--force]
                       [--root-mode 755] [--root-uid 0] [--root-gid 0] [--no-seed]
                       [--home-volume]
```

## Home volume (`--home-volume`)

`target/home.img` is the volume LazyOS mounts at `/home` (`home=LABEL=lazyhome`
in `lazyos.cfg`; filesystem plan F1/F2). `--home-volume` formats it: the label
defaults to `lazyhome`, and `<user>/` directories sit at the **volume root**
(the root is `/home` once mounted) with the owner and mode `/home/<user>` has
in the seeded layout (`0755`, that account's `uid:gid`; today `/alice`). There
is no `/home` and no `/tmp` inside it: `/tmp` belongs to the OS volume.
`run_demo.py` creates it on first use (`--home-disk`, `--no-home-disk`,
`--reset-home`), and the launcher's **Home volume** group manages it.

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
the `/system/etc/passwd` file `build.rs` puts on the OS volume ever differ. ids are limited
to 16 bits because that is all the kernel's ext2 driver stores.

## Persistence rule

The volume survives runs. `run_demo.py` and the launcher create it only when it
is missing and never regenerate it implicitly. Reset is explicit and confirmed:
`python tools/run_demo.py --reset-home` (or `--reset-data` for the legacy disk;
asks first, `--yes` to skip) or the launcher's **Reset volume** button, which
lists the directories it will create. A reset erases everything and rewrites
the layout. `--reset-os` is separate: it wipes the OS volume inside
`target/lazyos.img` (`LAZYOS_RESET_OS=1`) and leaves `home.img` alone.

## Tests

```bash
python tools/mkdisk/test_mkdisk.py   # geometry, driver-mount rules, mini fsck
python tools/mkdisk/test_seed.py     # ownership, sticky bit, free counts, CLI, account drift
python tools/lazygui/test_catalog.py # launcher plan flags and Reset
```

CI (`.github/workflows/mkdisk.yml`) also runs `e2fsck -fn` over the default
seeded image and several block sizes, root modes and multi-group sizes, and
`e2fsck`/`debugfs` over a `--home-volume` image (label, `/alice` owner and mode,
no `/home` or `/tmp`).

## Files

| File | Role |
|------|------|
| `geometry.py` | Block, group and inode placement from the requested size |
| `layout.py` | `Layout` / `DirSpec`: root attributes, the seeded and home-volume directories |
| `accounts.py` | Reads the demo accounts from `accountsd.rs` |
| `tree.py` | Directory inodes and blocks for the root and the seeded tree |
| `ext2.py` | Superblock, descriptors, bitmaps, `lost+found`; assembles the extents |
| `volume.py` | `format_image`, `ensure_volume`, `status` used by the launchers |
