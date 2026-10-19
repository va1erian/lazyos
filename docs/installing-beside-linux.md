# Installing LazyOS beside Linux

LazyOS can live on two partitions of a disk that already runs Linux and
start from that Linux's GRUB menu, with no stick plugged in. Linux keeps
booting exactly as before. One program, `lazyos-install`, does it from the
running Linux. The design and its decisions are
[`grub-install-plan.md`](grub-install-plan.md); this page is how to use it.

## What Linux must provide

You make these with Linux's own tools; the installer never creates, resizes
or repairs partitions, and never installs GRUB.

- **UEFI**, not BIOS/CSM (v1 refuses a BIOS boot), and **Secure Boot off**:
  the LazyOS loader and kernel are unsigned.
- **GRUB installed and working**, with an EFI System Partition (ESP) that has
  room for about the kernel's size plus 3 MiB (the installer checks and
  refuses rather than evicting anything).
- **Two empty GPT partitions on the same disk as the ESP**, one for the
  system (4 GiB recommended, at least the stick's OS volume and 1 GiB for the
  desktop) and one for `/home` (any size). Name them `lazyos-system` and
  `lazyos-home` and the installer finds them by name; otherwise pass
  `--system /dev/sdXN --home /dev/sdXM`. With `gdisk`: `c` (change name) on
  each. Shrinking the Linux root to make room is the usual first step, done
  from a live USB.

## Make the stick

```bash
python tools/run_demo.py --desktop --installer --build-only
```

`--installer` builds `lazyos-install` (`tools/install/build.py`, a static
musl program) and implies `--usb-image`; the stick maker
(`tools/boot/stick_gui.py`) has an *Installer (beside Linux)* box and the
launcher's Advanced tab a *...with lazyos-install* box that do the same.
Write `target/lazyos-usb.img` to a stick as [`usb-stick.md`](usb-stick.md)
says. The stick still boots on its own; its FAT partition now also has
`install/lazyos-install`, `install/40_lazyos.in` and `install/README.txt`.

## Install

Boot Linux, plug the stick in, and let the desktop mount it (say at
`/media/you/LAZYOS`). Desktops mount FAT without execute rights, so copy the
program out first:

```bash
cp /media/you/LAZYOS/install/lazyos-install /tmp/ && chmod +x /tmp/lazyos-install
```

```bash
sudo /tmp/lazyos-install --stick /media/you/LAZYOS --dry-run
```

```bash
sudo /tmp/lazyos-install --stick /media/you/LAZYOS
```

The dry run prints what Linux reported (distro, GRUB version, `/boot`'s
filesystem, Secure Boot), the partitions it chose with what they hold now,
the UUIDs it will give the new volumes, the `lazyos.cfg` and the GRUB entry,
and writes nothing. The real run prints the same and asks you to type the
system partition's name back before it formats anything. It then:

1. formats the system partition and copies the stick's OS volume onto it
   with the same code the image build uses (same layout, modes, owners and
   manifest), and formats the home partition with a private directory per
   account. An existing LazyOS home (label `lazyhome`) is kept unless you
   pass `--format-home`; `--home-from FILE` copies another home volume (the
   stick's, say) into a new one;
2. writes `\EFI\lazyos\` on the ESP: the loader `BOOTX64.EFI`, its
   `boot.json`, the kernel and a 2 MiB `ramdisk` that holds only
   `lazyos.cfg` (naming the two partitions by UUID), each through a
   temporary name, synced and renamed;
3. writes `/etc/grub.d/40_lazyos` (or, on a distro without `/etc/grub.d`, a
   marked block in GRUB's `custom.cfg`), runs `update-grub` or
   `grub-mkconfig`/`grub2-mkconfig`, and checks the generated `grub.cfg` has
   the entry;
4. runs the ext2 checker over both volumes (in memory up to
   `--check-limit` MiB, default 2048, else `e2fsck -fn`) and reads the ESP
   files back to compare their SHA-256.

It refuses, before writing anything: a mounted partition, the running
Linux's `/` or `/boot`, swap or LVM/RAID members, the ESP or any FAT volume
as a target, a partition on another disk than the ESP, the stick's own
disk, a system partition smaller than the OS volume, too little ESP space,
and a system partition that already holds LazyOS (use `update`).

## Try it once

```bash
sudo grub-reboot LazyOS && sudo reboot
```

(`--trial-boot` on the install does the `grub-reboot` for you.) That boots
LazyOS once; the boot after is Linux again, so a LazyOS that does not come up
costs one power cycle. Many distros hide GRUB's menu (`GRUB_TIMEOUT=0`); hold
Shift (BIOS-style) or press Esc (UEFI) at power-on to see it. The installer
does not change `/etc/default/grub`; if you want the menu always, set
`GRUB_TIMEOUT_STYLE=menu` and a timeout there and run `update-grub`.

## Update, status, uninstall

> Today `update` replaces the system in place, from Linux. The planned
> mechanism ([`update-plan.md`](update-plan.md)) installs updates from inside
> LazyOS, with one restart and automatic fallback to the previous release.
> It needs one more `update` from Linux, run with a stick that carries it.

```bash
sudo /tmp/lazyos-install update --stick /media/you/LAZYOS
```

`update` replaces the system from a newer stick in place, by the rules of an
image rebuild: what the stick's manifest lists is replaced or deleted,
everything else (`/conf` settings, `/apps` packages, `/logs`, anything you
made) stays, and `/home` is not touched. The kernel is written last. A
volume LazyOS did not shut down cleanly is checked and repaired first; damage
a crash cannot leave stops the update.

```bash
sudo /tmp/lazyos-install status
```

```bash
sudo /tmp/lazyos-install uninstall
```

`status` shows the partitions, the ESP files (and whether the kernel matches
the stick's) and whether GRUB has the entry. `uninstall` removes
`\EFI\lazyos\` and the GRUB entry and regenerates `grub.cfg`; the two
partitions are left for you to reuse.

## Recovery

- **LazyOS does not boot**: power off; without `grub-reboot` the next boot
  is Linux. From Linux, `lazyos-install status`, then `update` or
  `uninstall`.
- **An unclean power-off**: LazyOS's volumes are plain ext2 (no journal, on
  purpose: a journal Linux's ext2 driver would not replay is a trap), so
  `sudo e2fsck -f /dev/sdXN` from Linux repairs them, and `update` checks
  the system volume itself.
- **GRUB broken**: the stick still boots LazyOS on its own (it runs from its
  RAM root and does not touch the installed partitions' data), and Linux
  rescue media repair GRUB as usual. `efibootmgr` shows the firmware's boot
  entries.

## Testing without a PC

`lazyos-install --image disk.img` (test mode) runs every command against a
whole-disk image file instead of `/dev`: no root, no GRUB tools; the entry
goes into `<--grub-dir>/custom.cfg` on the image's ESP. Its host tests and
the end-to-end harness use it:

```bash
cargo test -p lazyos-install -p gpt
```

```bash
python tools/install/run.py
```

`tools/install/run.py` builds the stick, makes a Linux-shaped GPT disk
(`examples/testdisk.rs`: ESP, a Linux stand-in, the two LazyOS partitions),
installs into it, boots it **alone** on QEMU's AHCI controller under OVMF,
powers off from the Terminal, checks both volumes from the host and boots
again. `--via grub` (where `grub-mkstandalone` exists, as in CI) boots
through real GRUB with a Debian-shaped `grub.cfg`, its `next_entry` set as
`grub-reboot` does, and also checks that the boot after is the "Linux"
default; `--via shell` starts the loader from OVMF's shell exactly as GRUB's
`chainloader` does, on any host. `python tools/install/test_judge.py` shows
its judge fails when it should.
