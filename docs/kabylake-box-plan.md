# The Kaby Lake box (i3-7100U) — what it needs

> **Status: draft, revision 1 (2026-10-09). Nothing here is built.**
> The mini PC meant to be an Intel N150 with an I226-V
> ([n150-driver-plan.md](n150-driver-plan.md), [i226-driver-plan.md](i226-driver-plan.md))
> arrived as a 7th-generation Core i3 box with Realtek networking and a SATA
> SSD. This plan is the survey of that machine and the order of work; the two
> drivers it needs have their own plans: [ahci-plan.md](ahci-plan.md) (the
> disk) and [rtl8168-driver-plan.md](rtl8168-driver-plan.md) (Ethernet).
> Facts read from the box's Linux are stated plainly with their source;
> anything else is *to confirm* or *inferred*.

## 1. Short answer

1. **Two drivers, then the install.** The box should boot LazyOS from the USB
   stick with what exists today (UEFI, framebuffer, xHCI, LAPIC timer, ACPI
   S5, all built for the N150 plan; none yet run on real hardware). To *live*
   on the box it needs an **AHCI** driver (its only disk is a SATA SSD behind
   the PCH's AHCI controller; there is no NVMe) and an **RTL8111H** Ethernet
   back end for `netdrv`. The install path is the NVMe install plan's N2–N4
   with the disk named `ahci0` instead of `nvme0`.
2. **AHCI first.** QEMU models AHCI, so it is built and tested in CI without
   the box; it is also what turns the box from "boots a stick" into "boots
   itself". The Realtek driver has no QEMU model and is tested on the box.
3. **No audio for now.** The only HDA codec Linux found is "Intel Kabylake
   HDMI": no analog codec answered. HDMI audio needs a GPU driver, so the
   box is silent in v1 unless it turns out to have an analog jack whose codec
   Linux missed (§4, question 2).
4. **One small cross-cutting fix:** a driver that finds a device it cannot
   drive (an RTL8168 revision other than the one supported, an HDA controller
   with no analog path) must stop once and say so, not restart until the
   backoff gives up (§3.3).
5. **The I226 plan is deferred, not dropped**: nothing on this box uses it.

## 2. The machine

From `lspci -nn`, `lspci -vvv -s 02:00.0`, `dmesg` and `lsblk` on the box's
Linux (2026-10-09). A copy of that output belongs in `docs/compat/kabylake/`
(step B0).

| Function | Id | What it is | LazyOS today | Work |
|---|---|---|---|---|
| 00:00.0 | `8086:5904` | Kaby Lake host bridge | — | none |
| 00:02.0 | `8086:5916` | HD Graphics 620 | UEFI GOP framebuffer | none in v1 (no GPU driver: fixed mode, no HDMI audio) |
| 00:14.0 | `8086:9d2f` | Sunrise Point-LP xHCI | `usbd` (H3) | none expected; first real-hardware run of the xHCI handoff |
| 00:14.2 | `8086:9d31` | thermal subsystem | — | none |
| 00:16.0 | `8086:9d3a` | CSME HECI | — | none (never touched) |
| 00:17.0 | `8086:9d03` | SATA, AHCI mode, class `01:06` | **none** (ATA PIO is legacy IDE only) | **[ahci-plan.md](ahci-plan.md)** |
| 00:1c.0, 00:1c.4 | `8086:9d12`, `9d14` | PCIe root ports 3 and 5 | bridges walked | none |
| 00:1f.0 | `8086:9d4e` | LPC/eSPI | — | none |
| 00:1f.2 | `8086:9d21` | PMC | — | none (ACPI S5 goes through the FADT) |
| 00:1f.3 | `8086:9d71` | HD Audio, class `04:03` (legacy HDA, not DSP mode) | `sndd` matches it by class | §3.3: stop cleanly, the codec is HDMI only |
| 00:1f.4 | `8086:9d23` | SMBus | — | none |
| 01:00.0 | `10ec:c822` | RTL8822CE Wi-Fi | — | out (wifi-plan); stays Linux's under VFIO (§3.4) |
| 02:00.0 | `10ec:8168` rev `15` | RTL8168H/8111H, XID `541` (Linux's `r8169`) | **none** | **[rtl8168-driver-plan.md](rtl8168-driver-plan.md)** |

Platform facts that matter:

- **Disk:** `sda`, transport `sata`, model "SSD 512GB", 476.9 GiB, holding the
  Linux install today.
- **IOMMU:** the firmware has VT-d on (`DMAR` table, two remapping units,
  `DMAR-IR: Enabled IRQ remapping`). The Ethernet function is alone in IOMMU
  group 8 and the Wi-Fi in group 7, so passing the NIC to a LazyOS guest
  under QEMU/KVM on the box's Linux works (§3.4). The SATA controller is
  alone in group 3, but passing the only disk through is not a sensible loop.
- **UEFI:** Linux sees `efivars`, so the box boots UEFI (the stick's
  `\EFI\BOOT\BOOTX64.EFI` path).
- **Interrupts:** an I/O APIC (id 2) and an HPET exist; LazyOS uses the I/O
  APIC from the MADT and MSI/MSI-X for drivers
  ([interrupts.md](architecture/interrupts.md)).
- **Function count:** 15, well under the device table's 32.
- **CPU:** 2 cores / 4 threads; LazyOS runs on CPU 0 (SMP is platform S8).

## 3. What to build

### 3.1 AHCI — [ahci-plan.md](ahci-plan.md)

An in-kernel block driver (`libs/ahci` host-tested, `kernel/src/block/ahci.rs`
its machine side), shaped exactly like NVMe N1: polled, several commands in
flight, 512-byte logical sectors, flush and standby at power-off. QEMU's
AHCI controller (built into `-machine q35`) gives CI a boot-from-AHCI job.

### 3.2 RTL8111H — [rtl8168-driver-plan.md](rtl8168-driver-plan.md)

A fourth `netdrv` back end (`libs/rtl8168`), written from the BSD `re(4)`
drivers, never from Linux's GPL `r8169`. Supports exactly XID `541` in v1 and
refuses the rest by name. Tested with a host fake, then on the box through
VFIO, then bare metal from a cold boot.

### 3.3 Drivers that find nothing to drive

`init`'s driver rows are `Restart::Always` with a doubling backoff that gives
up after repeated rapid crashes (`libs/svcpolicy`). That is right for a crash
and wrong for "this is not a device I can drive": `sndd` on this box walks
the HDMI-only codec, gets `CodecError::NoPath`, exits, and is restarted until
the backoff gives up, logging each time. The RTL8168 back end has the same
shape for an unsupported XID.

Fix: one exit status, `EXIT_UNSUPPORTED` (a constant in `libs/svcpolicy`,
chosen so no other program uses it), that `decide` treats as final for any
row, and a `devd` state `unsupported` with the driver's one-line reason. The
reason reaches `devd` through `init` (which sees the exit) or a field on the
driver's existing records; the exact path is settled in the change, through
MIDL if it touches an interface. `devctl drivers` then shows
`unsupported: HDA codec has no analog output`. Tests: host tests in
`svcpolicy` and `libs/hda` (a fake codec with only a digital pin); one
headless run with an HDA controller whose codec has no line out
(`-device intel-hda -device hda-micro`, whose only output is a headphone
*to confirm*, otherwise the fake alone) that ends with exactly one `sndd`
start.

### 3.4 Real-hardware loop

- **VFIO for the NIC:** unbind `02:00.0` from `r8169`, bind it to
  `vfio-pci`, run `target/lazyos.img` under QEMU/KVM with `-device
  vfio-pci,host=02:00.0`; Linux keeps networking over the Wi-Fi. Caveat:
  Linux's `r8169` has already initialised the chip (PHY setup, a firmware
  patch if it loaded one), so a VFIO pass tests the rings and the MAC side,
  not a cold bring-up; that needs bare metal (rtl8168 plan §5).
- **The stick for everything else:** AHCI and the whole-system checks run
  from `lazyos-usb.img` on bare metal. Until the install (below) the SSD keeps
  Linux, and LazyOS writes nothing to it: the AHCI acceptance on the box
  reads only.

### 3.5 Installing to the SSD

The install plan's N2 (GPT reader), N3 (installer) and N4 (living on the
disk) apply once `ahci0` registers; that plan names `nvme0` only as its
example disk. The open choice is the disk: the install plan takes the whole
disk, which deletes the Linux this plan uses for VFIO and for collecting
facts. Recommended: keep Linux until the Realtek driver passes bare metal,
then decide between whole-disk LazyOS and shrinking Linux to leave room
(which needs N3 to install into free space, a small extension).

### 3.6 Later

From n150-driver-plan P2, unchanged in substance: the ACPI power button
(SCI), MWAIT idle (the hint still validated on the box), the PCH TCO
watchdog for update trial boots. None blocks the box being useful. Wi-Fi
(RTL8822CE: needs firmware and an 802.11 stack) stays with
[wifi-plan.md](wifi-plan.md).

## 4. Order and open questions

| Step | What | Needs the box |
|---|---|---|
| B0 | Save the survey output in `docs/compat/kabylake/`; first bare-metal boot of `lazyos-usb.img` (desktop, USB keyboard and mouse, `hwreport`, shutdown) | yes, an evening |
| B1 | AHCI A0–A3 (ahci plan) | no, until A4 |
| B2 | §3.3, the "unsupported" exit | no |
| B3 | RTL8168 R0–R4 (rtl8168 plan) | from R3 |
| B4 | Install (nvme-install-plan N2–N4 on `ahci0`) | yes |

1. Keep Linux on the SSD for now (recommended), or install LazyOS over the
   whole disk as soon as AHCI lands?
2. Does the box have a 3.5 mm audio jack? If it does, its codec did not
   answer Linux either (`cat /proc/asound/cards` and `dmesg | grep -i snd`
   would say why), and analog audio becomes its own item.
3. With a cable plugged in, what do `ethtool -i enp2s0` (the
   firmware-version field) and `sudo dmesg | grep -i rtl_nic` say? It decides
   whether Linux loads a PHY firmware patch for this chip (rtl8168 plan §2).
