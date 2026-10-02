# LazyOS on a USB stick

One image, `target/lazyos-usb.img`, written to a USB stick, boots a real PC in
UEFI mode (the primary target) or in legacy BIOS/CSM mode, and runs the whole
OS from RAM. A persistent `lazyhome` partition on the stick holds `/home`:
`usbd` serves the stick it booted from as a block device and the kernel mounts
that partition late ([`architecture/usb-storage.md`](architecture/usb-storage.md)). Plan: [`real-pc-boot-plan.md`](real-pc-boot-plan.md) H0.

## Build it

```bash
python tools/xui/build.py                        # the desktop apps (once, or after xui changes)
python tools/abi/busybox.py                      # the shell (once)
LAZYOS_DESKTOP=1 LAZYOS_USB=1 LAZYOS_USB_IMAGE=1 cargo build  # writes target/lazyos.img and target/lazyos-usb.img
```

`python tools/run_demo.py --desktop --usb-image` does the same (the launcher's
Advanced tab has a "USB stick image too" box). The stick image is opt-in:
`target/lazyos.img`, which dev runs and CI boot, is unchanged.

| Variable | Default | Effect |
|---|---|---|
| `LAZYOS_USB_IMAGE=1` | off | also write `target/lazyos-usb.img` |
| `LAZYOS_USB=1` | off | required with `LAZYOS_USB_IMAGE=1`: ships `usbd` and lists it in `init`'s manifest |
| `LAZYOS_USB_HOME_SIZE` | `1G` | size of the `lazyhome` partition (`16M` minimum; `K`/`M`/`G` suffixes) |
| `LAZYOS_USB_ROOT_FREE` | `64M` | free space left on the RAM root after the files are written |

**USB input is mandatory.** The target PC may have no PS/2 port, so the build
refuses `LAZYOS_USB_IMAGE=1` unless `LAZYOS_USB=1` and a services session
(`LAZYOS_DESKTOP=1`, or `LAZYOS_SERVICES=1`) are set, and checks that
`/system/bin/usbd` is in the file list (`usb_stick::check_profile`). `init`
then starts `usbd` after `inputd`. `run_demo.py --usb-image`, the launcher box
and `tools/boot/run.py` set both.

Every build writes a fresh stick image (new UUIDs, an empty home partition):
what persists lives on the stick, never in the file.

**Measured (desktop profile, this build):** the ramdisk is 104 MiB (38 MiB of
files plus the 64 MiB of free space, the FAT `/boot` and the MBR); the stick
image is 1134 MiB (1 GiB of it the home partition, sparse on the host). Writing
it adds 2.6 to 3.0 s to a build. Booting under QEMU without KVM (TCG, so far
slower than any PC), from `usb-storage` only:

| Firmware | Kernel entered | Desktop drawn (`SHELL:DESKTOP:PASS`) |
|---|---|---|
| OVMF (UEFI) | 5.7 s | 21.7 s |
| SeaBIOS | 65.8 s | 79.8 s |

The gap is the ramdisk load: the UEFI loader reads it through the firmware's
USB driver in large blocks, the BIOS loader through INT 13h in small ones.
Shrinking `LAZYOS_USB_ROOT_FREE` shortens the BIOS load almost linearly (the
free space is loaded too), at the cost of room for files written to `/`.

## Write it to a stick

Everything on the stick is erased.

```bash
python3 tools/boot/write_stick.py --list                 # removable/USB disks only, with size and model
sudo python3 tools/boot/write_stick.py --device /dev/sdX # Linux
python tools\boot\write_stick.py --device \\.\PhysicalDrive2   # Windows, in an Administrator prompt
```

The tool offers and accepts only removable or USB disks, refuses one with a
mounted partition on Linux (unmount it first) and the system or boot disk on
Windows (where it takes the chosen disk offline for the write, which dismounts
its volumes, and brings it back online after). It shows the model and size,
asks twice (the second time you type the device name back), writes the whole
image and reads it back to compare SHA-256 digests.

On Windows, [Rufus](https://rufus.ie) works too: pick the image and, when Rufus
asks, choose **DD Image** mode (not ISO mode). balenaEtcher writes raw images
as they are. Plain `dd if=target/lazyos-usb.img of=/dev/sdX bs=4M conv=fsync`
is the same thing without the checks.

## Boot it (ASUS PRIME Z890M-PLUS WIFI, AMI Aptio UEFI)

1. Enter the firmware setup (**Del** at power-on) and go to Advanced Mode (F7).
2. **Secure Boot off**: Boot > Secure Boot > OS Type = **Other OS**. If the
   firmware keeps enforcing, Key Management > **Clear Secure Boot Keys**. The
   kernel and the loader are not signed.
3. **Fast Boot off** (Boot > Fast Boot), so the firmware initialises USB before
   handing over.
4. UEFI mode needs nothing else. For legacy BIOS mode, enable **CSM** (Boot >
   CSM > Launch CSM = Enabled); the Z890 may not offer CSM at all, and UEFI is
   the supported path anyway.
5. Save, plug the stick in, reboot and press **F8** for the boot menu. Pick the
   entry named **UEFI: <stick model>** (or the plain stick name for BIOS mode).

The loader prints its progress on screen, then LazyOS starts. With a serial
port the kernel logs `BOOT:MEDIA:uefi` (or `bios`) and `FS:ROOT:ram0p2`.

## What persists, what does not

| Where | Lives on | Survives a power-off |
|---|---|---|
| `/` (programs, `/conf` settings, `/apps` installed packages, `/logs`, `/data`) | the RAM root, loaded from the stick at every boot | **no**: every boot starts from the image's state |
| `/boot` | the ramdisk's FAT volume (read-only) | n/a |
| `/tmp`, `/transient` | RAM | no |
| `/home` | the stick's `lazyhome` partition (MBR entry 3) | **yes** |

The kernel has no USB driver of its own: after the loader is done it reaches
the stick only through `usbd`, which serves it as `usb0` (`usb0p1` to
`usb0p3`). `lazyos.cfg` on the ramdisk says `home=LABEL=lazyhome`; the boot
does not find it (USB is not up yet), so `init` waits (bounded) while `usbd`
starts, and the kernel mounts `usb0p3` at `/home` late
(`fs: mounted usb0p3 at /home (late, home volume lazyhome)`, then
`INIT:HOME mounted`). `poweroff` syncs it before the machine stops. Pulling the
stick out while running loses only what was not yet synced; the OS itself
keeps running from RAM.

Booted in QEMU with `--media ide` or `--media virtio`, the kernel sees the whole
stick as a disk instead, still takes `/` from the ramdisk and mounts `/home`
from `lazyhome` at boot (read-only on IDE, whose driver cannot write).

## Image layout

The stick (MBR):

| Entry | Type | Content |
|---|---|---|
| 1 | `0x20` | `bootloader` BIOS stage 2, from LBA 1 |
| 2 | `0x0C`, active | FAT, label `LAZYOS`: `kernel-x86_64`, `ramdisk`, `boot-stage-3`, `boot-stage-4`, `efi/boot/bootx64.efi` |
| 3 | `0x83` | ext2 `lazyhome`, 1 MiB aligned after entry 2, the last partition |

Legacy BIOS runs the MBR code, stage 2 and stages 3/4; UEFI firmware treats
the stick as removable media, finds the FAT partition through the MBR and runs
`\EFI\BOOT\BOOTX64.EFI`, the `bootloader` crate's UEFI loader (copied out of
the crate by formatting a throwaway UEFI partition, `build_support/usb_image.rs`).
Both load the kernel and the ramdisk from the same partition. The build
rewrites two details of the FAT that `fatfs` 0.3 gets wrong for strict
firmware (`.`/`..` must be the first entries of a directory; the label must be
upper case, `build_support/usb_fat.rs`), so `fsck.fat -n` reports it clean.
OVMF boots MBR removable media; no GPT was needed.

The ramdisk (a whole-disk image, `build_support/usb_ramdisk.rs`):

| Entry | Start | Content |
|---|---|---|
| 1 | LBA 2048 | FAT12, 1 MiB: `lazyos.cfg` (`root=UUID=<RAM root>`, `home=LABEL=lazyhome`) |
| 2 | LBA 4096 | ext2 `lazyos`: the same directories and files as the OS volume of `target/lazyos.img`, sized to them plus `LAZYOS_USB_ROOT_FREE` |

The kernel registers the ramdisk as `ram0`, scans its MBR (`ram0p1`, `ram0p2`)
and mounts it through the ordinary configured layout; the ramdisk and its
partitions are searched before any disk, so `/` is always the RAM root even
when a disk carries a volume with the same UUID
([`architecture/block-devices.md`](architecture/block-devices.md)).

## Growing the home partition

The home partition is last on the stick, so it can grow into the rest of it.
The simplest way is to build the image with the size you want before writing it
(`LAZYOS_USB_HOME_SIZE=28G`; the file is sparse, the write takes longer). To grow
a stick already in use, on Linux (untested with LazyOS's ext2 driver; check
the volume afterwards and keep a copy of what matters):

```bash
sudo parted /dev/sdX resizepart 3 100%
sudo e2fsck -f /dev/sdX3
sudo resize2fs /dev/sdX3
```

## Testing in QEMU

`tools/boot/run.py` boots the image headless and judges it: `BOOT:MEDIA` must
name the firmware used, `FS:ROOT` must be a ramdisk partition, the desktop
marker must appear, and the screenshot must be a desktop (`pngstats.py`
numbers). It writes `shots/boot/<firmware>-<media>/{serial.log,screen.png,report.json}`;
read the PNG.

```bash
python tools/boot/run.py                              # OVMF, usb-storage on qemu-xhci only
python tools/boot/run.py --firmware bios --no-build   # SeaBIOS, usb-storage only
python tools/boot/run.py --media ide                  # the stick as an IDE disk (+ /home from lazyhome)
python tools/boot/run.py --media virtio               # as a legacy virtio-blk disk
python tools/boot/persist.py                          # /home survives power-off, OVMF and SeaBIOS
python tools/boot/test_run.py                         # the judges fail when they should
cargo test -p build-support-tests usb                 # the image layout, with the ext2 checker
```

`--media usb` attaches no IDE or virtio disk and no NIC; a USB keyboard and
mouse share the `qemu-xhci` controller with the stick, and the judge requires
`USBD:HID:KBD` (the target PC may have no PS/2 port).

`persist.py` builds the services profile (console login, BusyBox) with a 64 MiB
home partition and, per firmware, boots a copy of the stick twice from
`usb-storage` only: the first boot logs in as `alice`, writes a nonce to
`/home/alice/usbnote` and powers off; the second must find the volume clean,
read the nonce back and write a second file; then the host runs `e2fsck -fn`
on partition 3 and reads both files with `debugfs`. The console steps are the
USB storage harness's (`tools/storage/run.py`). Measured under TCG: each boot
and session takes 200 to 235 s, OVMF and SeaBIOS alike, and all checks pass.
OVMF comes from the distribution (`apt install ovmf`); the runner copies the
variable store per run. The image is opened `snapshot=on` unless `--persist`.
