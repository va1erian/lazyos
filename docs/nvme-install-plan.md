# Plan: install LazyOS on an NVMe drive (Intel N150 mini PC)

> **Status: draft proposal, revision 1 (2026-10-04).** Exploration only, no
> code yet. Builds on [`real-pc-boot-plan.md`](real-pc-boot-plan.md) (H0-H4,
> the USB stick), [`usb-stick.md`](usb-stick.md) (the stick image and its
> persistent `/home`), [`architecture/block-devices.md`](architecture/block-devices.md)
> (the block registry, MBR partitions, `lazyos.cfg` root selection),
> [`filesystem-plan.md`](filesystem-plan.md) (the OS volume layout, which
> lists AHCI/NVMe as "a later real-hardware track") and
> [`driver-plan.md`](driver-plan.md) D1 (block drivers are kernel drivers).
> Two sibling explorations run in parallel and are dependencies, not part of
> this plan: the **update mechanism** (how an installed system moves to a new
> build) and the **N150 driver package** (NIC, graphics, sound, Wi-Fi). Where
> they meet this plan is said explicitly below.

## Goal and scope

A small Intel N150 mini PC runs LazyOS full time. Its internal NVMe SSD holds
the whole system: the firmware boots LazyOS from it with no stick plugged in,
`/` (programs, `/conf` settings, `/apps` packages, `/logs`) and `/home` are
persistent ext2 volumes on it, and power-off, reboot and an unexpected power
loss leave it bootable. The USB stick becomes the installer and the rescue
medium, not the way the machine runs.

**Out (v1):** dual boot with the Windows the box shipped with (the installer
takes the whole disk), Secure Boot signing, disk encryption, swap, SATA M.2 or
eMMC (some N150 boxes have them instead of NVMe; AHCI and SDHCI are separate
drivers), more than one NVMe namespace, NVMe interrupts (polled, like
virtio-blk), and resizing an existing installation. Each has a seam below.

## What exists, what is missing

| Piece | State today | Needed for an NVMe install |
|---|---|---|
| Firmware boot | `target/lazyos-usb.img` boots UEFI (OVMF, a real Z890 board) from `\EFI\BOOT\BOOTX64.EFI` on an MBR FAT partition; the `bootloader` 0.11 UEFI stage loads `kernel-x86_64` (and an optional ramdisk) from the partition it was started from | the same loader and kernel on an ESP of the internal disk; the removable-media path also works on fixed disks on AMI Aptio, which N150 boxes use (inferred from the Z890 run and common practice; checked on the real box in N0) |
| NVMe | **none.** The kernel's block drivers are ATA PIO (read-only) and legacy virtio-blk; `usbd` serves USB sticks from user space through the block provider (syscall 33) | a kernel NVMe driver (N1) |
| Kernel MMIO and DMA | `mem::mmio::map_mmio` maps BAR pages uncached (used by the LAPIC); `block::virt_to_phys` and the virtio bounce region show the DMA pattern; PCI walks bridges and sizes 64-bit BARs | reused as is |
| Partition tables | MBR only (`block/partition.rs`); a protective `0xEE` entry is logged and skipped, so a GPT disk shows no partitions | a GPT reader (N2) |
| Root selection | `fs::mounts::build` finds a FAT volume carrying `lazyos.cfg`, mounts the ext2 `root=UUID=...` at `/` (on any device), `/boot` read-only, `home=` at `/home` | reused as is: the NVMe layout is the dev image's layout on another disk |
| FAT | FAT12/16 read-only in the kernel; FAT32 refused (#248) | the ESP is formatted FAT16 so the kernel can read `lazyos.cfg` from it (risk 2) |
| ext2 on a real disk | read/write, phase-ordered writeback (`libs/ext2fs/src/cache/flush.rs`), a 5 s flusher (`fs/flusher.rs`), sync on poweroff/reboot (`process/power.rs`), unclean volumes detected and mounted with a log line; no journal, no in-OS fsck | a recovery story for power loss (N4) |
| Image builder | `build_support/os_disk.rs` (fixed geometry OS volume), `usb_image.rs` / `usb_ramdisk.rs` / `usb_stick.rs` (the stick), `libs/ext2fs::format` (host and `no_std`) | `target/lazyos-nvme.img` (N2) |
| Writing a disk | `tools/boot/write_stick.py` writes removable/USB disks from a host | an installer that runs on the mini PC (N3) |
| Timer, input, display, power | LAPIC timer calibrated from ACPI (`arch/lapic.rs`, `timer_cal.rs`), xHCI handoff and hubs (H3), the logical screen and WC framebuffer (H1), ACPI S5 shutdown (`process/power.rs`) | nothing new for install; the N150's own quirks belong to the driver-package exploration |

The one hard missing piece is the NVMe driver. Everything else is a reuse of
what the stick already proved, plus a GPT reader and an installer.

## Key decisions

1. **NVMe is a kernel driver.** Driver-plan D1 puts block drivers in the
   kernel because the filesystem needs them before `init`. A user-space
   `nvmed` behind the block provider (the way `usbd` serves a stick) could
   only serve volumes mounted late, so `/` would stay a RAM root reloaded at
   every boot and `/conf` and `/apps` would be lost on power-off: fine for a
   stick, wrong for a machine that runs full time. The driver follows
   `virtio.rs`: polled, one I/O queue pair, one request in flight, a static
   bounce region of 64 KiB, every page translated with `virt_to_phys`.
2. **The installed layout is the dev image's layout, not the stick's.** The
   root is a real ext2 partition (`root=UUID=`), not a ramdisk, so nothing
   reloads at boot and writes to `/` persist. No ramdisk is handed over, so the
   ramdisk-wins rule never applies.
3. **GPT, one whole disk.** A fixed internal disk is where firmware is least
   forgiving about MBR in UEFI mode, and a 1 TB-plus SSD would need GPT
   anyway. The kernel learns to read GPT (header CRC, entry array CRC, backup
   header when the primary is bad), registering `nvme0p<n>` exactly as MBR
   entries are today.
4. **Install by streaming a tested image.** The build writes
   `target/lazyos-nvme.img`; the installer copies it to the disk byte for
   byte and then fits it to the disk (backup GPT header, a fresh home volume
   filling the rest). The bytes that boot the mini PC are the bytes CI booted
   under OVMF on an emulated NVMe, instead of a file-by-file copy that only
   the real machine ever runs.
5. **The layout is the update mechanism's, from day one.** The update
   exploration (`docs/update-plan.md`, drafted in parallel) proposes
   image-based A/B slots: per slot a small FAT boot partition and an ext2
   system partition mounted read-only at `/system`, one persistent ext2
   state volume at `/` (`/apps`, `/conf`, `/logs`), and a raw `lazyboot`
   partition recording the active slot. The installer writes that layout even
   while only slot A is used, so a first update never repartitions a disk
   that holds someone's home.

## Disk layout

GPT, 1 MiB aligned, sizes for a 256 GB or larger SSD. Partitions marked
*update* follow the update plan's proposal and change with it:

| # | Type | Size | Format, label | Content |
|---|---|---|---|---|
| 1 | ESP `C12A7328-...` | 64 MiB | FAT32 or FAT16, `LAZYESP` | `EFI/BOOT/BOOTX64.EFI`: the update plan's boot shim (a forked `bootloader` UEFI stage) that reads `lazyboot` and loads the kernel from `boot_a` or `boot_b` |
| 2 | *update* raw | 1 MiB | `lazyboot` | slot state: active slot, tries left, good flags (two checksummed copies) |
| 3 | *update* | 64 MiB | FAT16, `boot_a` | `kernel-x86_64`, `lazyos.cfg` (`root=UUID=<state>`, `home=UUID=<home>`, the slot's system volume) |
| 4 | *update* Linux fs | 2 GiB | ext2, `system_a` | `/system`, `/docs/os`, read-only |
| 5 | *update* | 64 MiB | FAT16, `boot_b` | empty until the first update |
| 6 | *update* Linux fs | 2 GiB | `system_b` | empty until the first update |
| 7 | Linux fs `0FC63DAF-...` | 8 GiB | ext2, `lazystate` | `/`: `/apps`, `/conf`, `/logs`, `/data` |
| 8 | Linux fs | the rest | ext2, `home` | `/home` |

**Before the update plan lands** (no boot shim, no state/system split yet),
N2 writes the same table but uses only partitions 1, 3, 7 and 8: the ESP
holds the plain `bootloader` UEFI stage, which loads `kernel-x86_64` from its
own partition, so the kernel and `lazyos.cfg` sit on the ESP (formatted
FAT16, since the kernel reads FAT12/16 only, risk 2) and the whole OS volume
of today, `/system` included, is partition 7. Moving `/system` to
`system_a` and the kernel to `boot_a` is then a rewrite of partitions that
already exist, with `/home` and the state volume untouched.

`home=` names a UUID and the label is not `lazyhome`: the stick names its
home `LABEL=lazyhome` and searches every disk for it, so with N1 a stick
booted on the installed machine would otherwise mount the SSD's home, and an
installed machine booted with the stick plugged in could pick the stick's.
The stick build should move to `home=UUID=` too (one line in
`usb_ramdisk.rs`).

Boot chain (with the update plan's shim; before it, the ESP's loader loads
the kernel from the ESP itself):

```
 UEFI firmware ─► ESP: \EFI\BOOT\BOOTX64.EFI (boot shim)
                   └─ reads lazyboot, loads kernel-x86_64 from boot_a (no ramdisk)
 kernel: PCI ─► nvme0 attached (N1) ─► GPT: nvme0p1..p8 (N2)
         ─► FAT boot_a carries lazyos.cfg ─► / = ext2 lazystate (rw)
         ─► /system = system_a (ro), /boot = boot_a (ro), /home ─► init ─► desktop
```

## Phases

Each is shippable alone and testable in QEMU, which has an NVMe controller
(`-device nvme`). Kernel phases ship correctness and stress suites under
`kernel/src/tests/` and pass `python tools/test/run.py --accel none`
(AGENTS.md).

| Phase | Deliverable | Tests and evidence |
|---|---|---|
| **N0** Recon on the real box | Boot the existing stick on the N150 and record its `hwreport` in a compatibility row: the NVMe controller (PCI id, `CAP.MQES`, `MDTS`, LBA format), firmware boot menu key, whether Secure Boot can be turned off, whether the desktop and USB input come up. No code. Shared with the driver-package exploration, which needs the same inventory | a photo and the `hwreport` text |
| **N1** Kernel NVMe driver | `kernel/src/block/nvme.rs` (+ `nvme/` submodules under 500 lines each): match class `01:08:02`, map BAR0 with `map_mmio`, enable bus mastering, controller reset (`CC.EN=0`, wait `CSTS.RDY=0` bounded by `CAP.TO`), admin queue (`AQA/ASQ/ACQ`), Identify controller and namespace 1, one I/O completion and submission queue (Create I/O CQ/SQ, polled, phase bit), Read/Write with PRP1 and a PRP list for up to 64 KiB (capped by `MDTS`), Flush when the controller reports a volatile write cache, normal shutdown notification (`CC.SHN`, wait `CSTS.SHST`) from the power path after the filesystem sync. Doorbell stride from `CAP.DSTRD`. Every wait bounded; a controller that times out or reports `CSTS.CFS` is detached and logged, never hangs boot. Only 512-byte LBA formats are served (the block layer is 512 everywhere); a 4 KiB-formatted namespace is refused with a log line naming it. Registered in the device-core driver table after virtio, never displacing an earlier boot device | `nvme_suite`: identify parsing and a refused 4 KiB format against a fake register file (host-testable core in a pure module, seeded fuzz for the identify and completion parsers); in QEMU: read/write/flush round trips, every PRP shape (1 page, 2 pages, a list, an unaligned buffer), a 64 KiB vectored ext2 writeback, a soak of random reads and writes checked against a shadow copy, a controller that never becomes ready. `tools/boot/run.py --media nvme` |
| **N2** GPT and the NVMe image | `block/gpt.rs`: header at LBA 1 (signature, revision, header size, CRC32, `my_lba`), entry array CRC, backup header at the last LBA when the primary fails, entries bounded and non-overlapping, known types registered as `<disk>p<n>`; the MBR path unchanged for MBR disks. `build_support/nvme_image.rs` behind `LAZYOS_NVME_IMAGE=1`: writes `target/lazyos-nvme.img` in the layout above from the same kernel and OS file list, with the `bootloader` UEFI application copied as `usb_image.rs` does and the ESP formatted FAT16 (partitions 2 to 6 written empty until the update plan uses them) | `partition_suite` grows GPT cases: a valid table, a bad primary with a good backup, both bad, a CRC mismatch, an entry past the disk, overlapping entries, 128 entries, a hybrid MBR. `cargo test -p build-support-tests nvme` checks the layout with the ext2 checker and `fsck.fat -n`. `tools/boot/run.py --firmware uefi --media nvme`: OVMF with the image on `-device nvme` and nothing else; `FS:ROOT:nvme0p7` required, the desktop judged by `pngstats.py`. A persistence run modelled on `persist.py`: a file written to `/conf` and `/home`, poweroff, second boot reads both, `e2fsck -fn` on partitions 7 and 8 |
| **N3** The installer | A `lazyinstall` command on the stick, run as an admin from the Terminal. It lists non-removable disks only (never the boot stick, never a disk with a mounted volume), shows model, serial and size, asks twice (the second time the user types the model back, like `write_stick.py`), streams the NVMe image from a fourth partition of the stick (`lazyinst`, written by the stick build when `LAZYOS_NVME_IMAGE=1`) to the disk, verifies it by reading back and comparing SHA-256, writes the backup GPT header at the disk's end, grows entry 8 (home) to fill the disk and formats it with `ext2fs::format`, and offers to copy the stick's `/home` across. Raw disk writes need a new kernel interface, shared with the update plan's updater (which writes whole partition images to the inactive slot): a `CAP_BLOCK_RAW` grant naming the devices or partitions it covers, given by `init` only to `lazyinstall` (the whole disk) and the updater (the inactive slot's partitions), refused on any device with a mounted volume or a partition of one, every write bounds-checked against the device | QEMU: the stick on `usb-storage` plus an empty `-device nvme` disk; a scripted session runs `lazyinstall`, then the VM reboots from the NVMe disk alone and must reach the desktop with the copied home. Refusal paths: the stick itself, a disk with a mounted volume, a wrong typed model, a disk smaller than the image |
| **N4** Living on the disk | Firmware notes for the N150 (boot order, Secure Boot off, Fast Boot off) in a `docs/nvme-install.md` user guide; an on-screen warning when `/` mounted unclean, and a rescue path: boot the stick, which mounts nothing from the NVMe disk (its `lazyos.cfg` names its own UUIDs), and run an `ext2check` built from the host's offline repair (`build_support/os_recover.rs`) against `nvme0p7`/`p8`. Measured: boot time to desktop from NVMe, sustained write throughput, and what an unexpected power cut costs (at most the flusher's 5 s, by design) | the persistence harness with a hard `quit` from QEMU's monitor instead of a poweroff, repeated: the next boot must mount, report unclean, and `ext2check` must leave a clean volume |
| **N5** Graphical installer (later) | An "Install LazyOS on this PC" flow in the desktop, on top of `lazyinstall`'s logic (the Installer app today installs packages; a separate app is fine), showing the disk, the layout and progress | screenshots of each step in QEMU, read |

**Suggested order:** N0 now (one evening with the stick). N1 is the critical
path and the only new kernel subsystem; N2 follows and already gives a manual
install route (below). N3 and N4 make it something a person can repeat
without a host computer; N5 is polish.

### A manual install before N3

Once N2 lands, `target/lazyos-nvme.img` can reach the SSD without an
installer, by either:

1. putting the SSD in a USB M.2 NVMe enclosure, which `write_stick.py` already
   accepts as a USB disk, writing the image, and putting the SSD back; or
2. booting any Linux live stick on the mini PC and running
   `dd if=lazyos-nvme.img of=/dev/nvme0n1 bs=4M conv=fsync`, then
   `sgdisk -e /dev/nvme0n1` to move the backup GPT header to the disk's end.

Either leaves the home partition at the image's size (`LAZYOS_NVME_HOME_SIZE`
picks it at build time, as `LAZYOS_USB_HOME_SIZE` does for the stick).

## Dependencies on the sibling explorations

- **Update mechanism.** This plan adopts its proposed layout (above) and
  needs from it the boot shim and the state/system split; until they land the
  install uses partitions 1, 3, 7 and 8 only. It gives it the NVMe driver and
  `CAP_BLOCK_RAW`, which the updater uses to write the inactive slot. Both
  write partition images, so the installer and the updater can share the
  streaming and verification code. Because slots are whole partition images,
  neither needs a FAT writer in the kernel.
- **N150 driver package.** That exploration owns the shared driver
  infrastructure (MSI through the LAPIC, PCIe ECAM from the ACPI MCFG table)
  and AHCI as the fallback for SATA M.2 boxes; this plan owns the NVMe
  controller driver (N1), polled first, moving to MSI once that lands. The
  install needs nothing else from that package: the GOP framebuffer, the
  LAPIC timer and USB input already work on real PCs. Networking (Intel
  I226-V or Realtek RTL8125 on most N150 boxes, inferred) matters for
  updates, not for the install.

## Risks and open questions

1. **The SSD's LBA format.** Consumer NVMe drives ship formatted with 512-byte
   LBAs almost always; a 4 KiB-only namespace is refused in v1, and serving it
   means teaching the block layer a sector size other than 512. N0 reads it.
   Reformatting a namespace (Format NVM) is destructive and stays out.
2. **A FAT16 ESP before the boot shim.** The kernel reads FAT12/16 only, so
   while `lazyos.cfg` sits on the ESP the ESP must be FAT16. The UEFI
   specification names FAT32 for system partitions; EDK2-derived firmware
   (AMI Aptio included) reads FAT12/16/32 on any disk, and the stick's FAT
   partition already boots that way, but a fixed-disk FAT16 ESP is unproven
   on the N150. With the shim, `lazyos.cfg` moves to `boot_a` and the ESP can
   be FAT32. Until then, if the firmware refuses FAT16, the fallback is FAT32
   read support in the kernel (#248).
3. **No boot entry is written.** The `bootloader` UEFI stage exits boot
   services, so LazyOS cannot add an NVRAM boot entry; it relies on the
   firmware listing the disk's `\EFI\BOOT\BOOTX64.EFI` as "UEFI OS". The user
   sets the boot order once in setup, and the guide says how.
4. **Power loss on a machine that never stops.** ext2 has no journal. The
   flusher bounds the loss to about 5 s and writeback is phase ordered, but a
   cut during a commit can leave a volume that needs repair; v1's answer is
   the rescue stick (N4). A journal or a boot-time check is the next step
   after N4 and belongs in the filesystem plan.
5. **NVMe power management.** Firmware may leave Autonomous Power State
   Transitions on; a polled driver that issues commands rarely may see first
   request latency from deep states. The driver does not touch APST in v1;
   N4 measures it.
6. **Raw disk access is new authority.** `CAP_BLOCK_RAW` is the first way for
   user space to write a whole disk; it must be grantable only by `init` to
   `lazyinstall`, logged on every grant, and refused on mounted devices. The
   security model gets a section for it in N3.
7. **SATA or eMMC instead of NVMe.** Some N150 boxes ship either. N0 tells; if
   the box is one of them this plan's N1 becomes AHCI or SDHCI, and N2 to N5
   are unchanged.
