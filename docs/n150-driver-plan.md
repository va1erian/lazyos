# Drivers for Intel N150 mini PCs — exploration and plan

> **Status: exploratory, revision 1 (2026-10-04). Nothing here is built.**
> One question: what does LazyOS need, driver-wise, to run full time on a small
> Intel N150 ("Twin Lake") mini PC? Companion plans own the neighbouring
> pieces: installing to the internal NVMe disk and updating an installed system
> are their own threads of work, and this plan only names where it depends on
> them. Facts checked on the date above are marked *verified*; anything that
> depends on the exact board, or that could not be checked from here, is marked
> *inferred* or *to confirm*.

Related: [driver-plan.md](driver-plan.md) (the device core, D7 is the
"second driver per class" stage this plan mostly fills),
[real-pc-boot-plan.md](real-pc-boot-plan.md) (H0–H4, what already boots a real
PC), [usb-stick.md](usb-stick.md), [networking-plan.md](networking-plan.md),
[audio-plan.md](audio-plan.md), [wifi-plan.md](wifi-plan.md),
[architecture/devices.md](architecture/devices.md),
[architecture/usb.md](architecture/usb.md).

## 1. Short answer

1. **Most of the machine already works from the USB stick, in principle.**
   UEFI boot, the GOP framebuffer (write-combined, logical screen), the LAPIC
   timer with ACPI calibration (the PIT is clock-gated on Intel parts this
   recent), the xHCI controller with keyboards, mice, hubs and USB storage,
   and ACPI S5 shutdown are all built (real-PC plan H0–H4). None of it has a
   real-hardware row yet (there is no `docs/compat/hardware.md`); the N150
   box would be the first, which is step K0 below.
2. **The missing drivers, in the order a full-time box needs them:**
   internal storage (NVMe, owned by the install work; AHCI as a fallback),
   **MSI interrupts** (so PCIe devices stop being polled), **one wired
   Ethernet driver** (Realtek or Intel, decided by what is on the board),
   **Intel HDA audio** (analog jack), an **ACPI power button** and a
   **MWAIT idle loop** (a box that runs all day should sleep its cores and
   shut down cleanly when the button is pressed). Wi-Fi and the GPU stay out
   of v1; §5 says why and what that costs.
3. **"Driver package" means base-image drivers matched by PCI id, not an
   `.lzp`.** A driver holds `CAP_DEV_CLAIM` and, with DMA, is trusted like
   the kernel (driver-plan D5); application packages cannot name
   `os.kernel.dev` at all (packages.md, core packages). So the package is a
   set of userspace drivers in the image plus match rules, started only when
   their device is present, and delivered to an installed box by the update
   mechanism. That needs `devd`, which landed with issue #497
   ([architecture/drivers.md](architecture/drivers.md)).
4. **First action is a survey, not code.** Mini PCs with the same CPU ship
   different NICs, M.2 wiring and Wi-Fi modules. Boot the stick on the box,
   run `devctl` and `dmesg`, and the vendor:device list decides which NIC
   driver to write.

## 2. The machine

### 2.1 What the N150 SoC brings (same on every board)

| Block | What it is | LazyOS today |
|---|---|---|
| CPU | 4 Gracemont E-cores, no SMT, x86-64-v3 (AVX2) | runs on CPU 0 only (SMP is platform S8) |
| GPU | Intel UHD (Xe-LP), Alder Lake-N family, PCI `8086:46D0`–`46D4` *verified* in Linux `include/drm/intel/pciids.h` (ADLN); which id the N150 reports is *to confirm* | GOP framebuffer only, no driver |
| USB | PCH xHCI (`8086:54ED` *inferred*) | `usbd` drives it (H3); BIOS handoff built |
| Audio | PCH HDA controller (`8086:54C8` *inferred*), a Realtek codec on the board, Intel display codec for HDMI/DP | none (`sndd` is virtio-snd) |
| SATA | PCH AHCI controller, when the board wires one | none (ATA PIO is IDE-only) |
| PCIe | 9 Gen3 lanes shared between the M.2 slot(s), the NIC(s) and Wi-Fi | config mechanism 1, no ECAM, INTx only |
| Wi-Fi | CNVi: the MAC is in the PCH, the M.2 module (typically Intel AX101) is the radio | none |
| Timer / ACPI | LAPIC, TSC, HPET (often hidden), ACPI PM timer | LAPIC timer calibrated from the PM timer (H2) |
| Platform | LPSS (I2C, UART, SPI), eSPI to the embedded controller, which runs the fan | not needed: the EC runs the fan with no OS help |

### 2.2 What varies by board (decides the driver list)

| Part | Common choices on N150 mini PCs | Driver it needs |
|---|---|---|
| Disk | M.2 2280 NVMe; on some boards the same slot also takes M.2 SATA (Beelink EQ14: "M.2 PCIe Gen3 x4 / SATA III socket" *verified*, CNX Software); a few cheap boxes add eMMC | NVMe; AHCI for SATA; SDHCI for eMMC |
| Ethernet | Realtek RTL8111H (1 GbE) or RTL8125B (2.5 GbE), Intel I226-V (2.5 GbE) on firewall-style boxes *inferred from vendor listings* | one of `rtl8169`-family or `igc`-family |
| Wi-Fi | Intel AX101 (Beelink EQ14 *verified*), AX201/AX211 class on others | CNVi `iwlwifi`-class |

## 3. What LazyOS needs, by priority

### P0 — Platform

> **Built (issue #616):** MSI and MSI-X for userspace drivers and the I/O
> APIC for the legacy lines, [architecture/interrupts.md](architecture/interrupts.md).
> MSI-X came with it because virtio has nothing else; the I/O APIC is the
> default controller, PCI INTx on it follows the firmware's ISA routing
> (no `_PRT`). The paragraphs below are the plan as written.

**MSI through the local APIC.** Today every interrupt arrives through the 8259
in virtual-wire mode, and a PCI function whose Interrupt Line is unroutable
falls back to polling (driver-plan §3.3). Under UEFI-only firmware the
Interrupt Line of a PCIe device is often not a usable PIC line (*inferred*:
CSM-era firmware programmed PIRQ routing; native UEFI need not), so on the N150
every new driver would likely poll. Polling works, but costs latency (the tick
is 100 Hz) and keeps a core awake.

MSI is the shortest fix and needs no IOAPIC: an MSI is a memory write to the
LAPIC (`0xFEE0_0000 | dest`, data = vector), and H2 already enables the LAPIC.
The work is: an `Irq::Msi` resource (the seam in `dev/resources.rs` exists),
a block of IDT vectors above the PIC range whose stubs reuse
`dev::irq::dispatch`'s raised-bit and bottom-half path, programming the MSI
capability through `cfg_*` (kernel-side: userspace still never writes an
address register), and LAPIC EOI instead of PIC EOI on those vectors. The
driver-facing contract (`irq_enable`, the one-way message, `irq_ack`) does not
change. MSI-X (NVMe and `igc` prefer it, both accept MSI) follows the same
path with a table in a BAR the kernel maps itself. IOAPIC routing stays an S8
item.

**ECAM (MMCONFIG) from the ACPI MCFG table.** Not required by any driver
below (MSI, MSI-X, BARs and the PCIe capability all sit in the first 256
bytes), so it is P2: it unlocks extended capabilities (ASPM/L1 substates,
LTR) that matter for idle power, and is a small `libs/acpi` table plus a
second config accessor.

**`devd`.** Matches devices to drivers and asks `init` to start them
(driver-plan §3.6). With one virtio NIC and one virtio sound card, `init`
could start every driver unconditionally; with three NIC families and two
audio paths it cannot sensibly. `devd` is also what makes "the N150 package"
a list of manifest rows rather than a build flag (real-PC decision 4: one
image boots everywhere).

### P0 — Storage (dependency, not owned here)

The NVMe controller driver is part of the NVMe-install work
([nvme-install-plan.md](nvme-install-plan.md), PR #564, phase N1: polled, one
queue pair, 512-byte LBAs): the root volume must be readable before `init`,
so it is an in-kernel block driver (driver-plan D1). It moves to MSI once K1
lands. Because N1 serves 512-byte LBAs only, the box is listed as
NVMe-compatible only once its namespace's active LBA format is recorded in
its compat row and is 512 bytes (some drives ship formatted 4096): N1 can log
it at probe, and until then `nvme id-ns -H` from a Linux live stick shows it.
This plan links to N1 rather than duplicating it and only adds:

- **AHCI** for a box whose M.2 or 2.5" bay holds a SATA disk. In-kernel for
  the same reason, ports polled, NCQ optional. QEMU models it (`ich9-ahci`),
  so it is fully testable before any hardware.
- **eMMC (SDHCI)** only if the surveyed box boots from eMMC; otherwise a
  non-goal.

### P1 — Wired Ethernet

One userspace driver serving `os.lazy.net.nic.v1` unchanged, so `netd`, the
socket layer and the tools need nothing. This is exactly driver-plan D7's
"e1000 with no new syscall ops" test, on hardware that matters.

| Chip family | Reference to write from | QEMU model | Notes |
|---|---|---|---|
| Intel I225/I226 (`igc`) | Intel's public I225/I226 datasheet; FreeBSD `igc(4)` (BSD-2-Clause) | none (QEMU has `e1000e`/`igb`, related descriptor format) | legacy or advanced descriptors, MSI-X preferred, MSI accepted *inferred* |
| Realtek RTL8111/8125 | no public datasheet; FreeBSD `re(4)` and OpenBSD `re(4)`/`rge(4)` (BSD) | none (QEMU has only `rtl8139`) | many chip revisions with per-revision PHY setup; RTL8125 may want a PHY firmware blob on some revisions *to confirm* |

The Intel family has its own plan: [i226-driver-plan.md](i226-driver-plan.md).

Licence: Linux's `r8169` and `igc` are GPL-2.0-only, which LazyOS
(GPL-3.0-or-later) cannot take (wifi-plan §4 has the reasoning). The BSD
drivers and Intel's datasheet are usable; the Rust driver is written fresh in
LazyOS's own shape.

Shape, as `netdrv` already does it: a host-tested `no_std` library (rings,
descriptor parsing against a hostile device, link state) plus a small binary
that claims the function. `libs/nicdrv`'s engine (client rings, receive
filter, stats) is device-neutral in spirit but takes virtio `Queues`; the
first new NIC splits it into an engine over a `DescriptorRing` trait, with
virtio as one implementation. Because neither real chip has a QEMU model, the
library carries a register-level fake device for host tests, and the
end-to-end proof is the real box: DHCP lease, `ping`, `curl https://…`.

### P1 — Audio (Intel HDA)

An `sndd-hda` userspace driver serving `os.lazy.audio.v1`, so `audiod` and
every client keep working. The controller side (CORB/RIRB command rings,
stream descriptors with buffer descriptor lists) is well documented by Intel's
public HDA specification, and QEMU models it (`-device intel-hda -device
hda-duplex`), so it can be built and WAV-verified headless exactly like
`sndd` (`tools/sound/run.py`). The codec side is the board-specific part:
walk the widget graph from the codec's verbs, find the headphone/line-out pin
and its DAC, unmute, set the stream format. A generic parser covers most
Realtek ALC codecs; quirks show up only on the real box.

Two risks: the HDA function may be presented in DSP (SOF) mode instead of
legacy HDA on some firmware (Linux decides this per platform, defaulting to
legacy when no digital microphone is present *inferred*; a mini PC has
none), and **HDMI/DP audio is out**: the display codec needs the GPU driver
to power the display audio well.

### P2 — Running all day

- **ACPI power button.** The fixed power button raises `PWRBTN_STS` in PM1
  status and an SCI (FADT `SCI_INT`, usually IRQ 9, level, shared). The FADT
  is already parsed; the work is a kernel SCI handler on the PIC line that
  clears the status bit and posts an event `init` turns into its existing
  shutdown path. Without it, the only clean shutdown is from the desktop or a
  shell.
- **MWAIT idle.** The idle loop is `hlt`. CPUID leaf 5 only counts
  processor-specific MWAIT sub-states; which ones the platform actually
  exposes, and their wake latencies, come from firmware (ACPI `_CST`, which
  LazyOS cannot evaluate without an AML interpreter) or from a per-model
  table such as Linux `intel_idle` keeps. So: K0 records the box's CPUID
  leaf 5 and the hints its `_CST` names (read once from a Linux live stick);
  entry arms `MONITOR` on a per-CPU wake line before ordinary `MWAIT`, or
  uses monitorless `MWAIT` only when `CPUID.05H:ECX[3]` reports it; the
  first cut uses only a hint validated on this box (wakes on the next tick
  and on device interrupts, timekeeping unchanged), falling back to `hlt`
  when the hint or an entry path is missing; deeper states join a small N150
  table only after the same check.
  Measured win needs a power meter; *inferred* to matter for a 6 W part
  that idles most of the day.
- **Hardware watchdog (PCH iTCO).** The update work
  ([update-plan.md](update-plan.md), on its own branch) wants a hung trial
  boot of a new release to reboot by itself and fall back to the previous
  slot. The PCH TCO timer does that: arm it early in boot, have `init` pet it
  once the system is healthy, stop petting on a failed trial. It is a few
  I/O registers behind the SMBus/LPC function (TCO base from the PCH, the
  `NO_REBOOT` bit in the PMC) *to confirm on the box*; a kernel driver
  because it must run before userspace.
- **Restore on AC loss / wake on LAN** are firmware settings, documented in
  the install guide, not drivers.
- **Fan and thermals** are run by the embedded controller; no driver.
  Reading the package temperature (`IA32_PACKAGE_THERM_STATUS`) for
  `sysmon` is a nice small extra.

### P3 — Later or out

| Item | Why not now | What it costs to skip |
|---|---|---|
| GPU driver (i915/Xe class) | very large; GOP framebuffer already gives a desktop | resolution fixed at boot, monitor must be connected at boot, one display, no HDMI audio |
| Wi-Fi (CNVi AX101) | needs an 802.11 station stack (wifi-plan W-stages) and Intel firmware; Linux `iwlwifi` is dual GPL-2.0/BSD-3-Clause per file headers *to confirm*, but sits on GPL-only `mac80211` | use the Ethernet port; wifi-plan's USB dongle path comes first |
| Bluetooth | no stack | none for a desktop box |
| SMP | platform S8 | three of four cores idle |
| IOMMU (VT-d) | driver-plan D5 named stage | DMA drivers remain kernel-trusted |

## 4. Packaging and delivery

- **What ships:** the drivers above as ordinary programs in the base image
  (`/system/bin`), each with a dedicated system uid holding only
  `CAP_DEV_CLAIM` (as `_net`, `_snd`, `_usb` do), and one `devd` manifest
  row each: PCI vendor:device (or class) to program, uid, restart policy.
  In-kernel block drivers (NVMe, AHCI) are in the static `DRIVERS` table.
- **What "N150 support" is:** the set of rows whose ids appear on N150
  boards, plus a `docs/compat/hardware.md` row per tested box. No hardware
  build switch, no separate image.
- **Not an `.lzp`:** an application package cannot be granted device claims,
  and making one able to would let a downloaded archive install a
  DMA-capable, kernel-trusted program. If third-party drivers are ever
  wanted, that is a new, signed package kind with its own review; out of
  scope.
- **Delivery to an installed box:** through the update mechanism (sibling
  work). A driver update replaces a program and a manifest row; an in-kernel
  driver update is a kernel update.

## 5. Stages

Each stage is mergeable alone and follows AGENTS.md (correctness and stress
tests, `python tools/test/run.py --accel none`, files under 500 lines).

| Stage | Deliverable | Verified by |
|---|---|---|
| **K0** Survey (same as the install plan's N0) | Boot `lazyos-usb.img` on the box; record `devctl`, `dmesg` and the `HW:*` lines; from a Linux live stick, the NVMe namespace's active LBA format, CPUID leaf 5 and the `_CST` hints; open `docs/compat/hardware.md` with the first row | photos and the copied text; decides K2's chip family, NVMe compatibility with N1, and the K4 idle hint |
| **K1** MSI + `devd` | `Irq::Msi`, MSI vectors and LAPIC EOI, kernel-programmed MSI capability; `devd` with a static manifest moving `netdrv` and `sndd` to device-matched starts | QEMU q35: virtio-net and virtio-snd on MSI (`DEV:MSI:PASS`), storm and teardown tests as for INTx; `devd` starts nothing on a machine without the device |
| **K2** Ethernet | `libs/<chip>` + `netdrv-<chip>` for the surveyed NIC; `nicdrv` engine split over a ring trait | host tests with a fake device; on the box: DHCP, `ping`, HTTPS fetch, a 1 GiB transfer without loss |
| **K3** HDA | `libs/hda` + `sndd-hda`, generic codec parser | QEMU `intel-hda` WAV test like `tools/sound/run.py`; on the box: tone on the headphone jack |
| **K4** Power | SCI power button to `init` shutdown; MWAIT idle; iTCO watchdog for the update trial boot; optional package temperature in `sysmon` | QEMU `system_powerdown` (ACPI power button event) ends in a clean shutdown; on the box: button press |
| **K5** AHCI | in-kernel AHCI block driver | QEMU `ich9-ahci` with an ext2 disk; skipped if the box is NVMe-only and nobody needs it |
| **K6** Wi-Fi | per wifi-plan | per wifi-plan |

Order: K0 first because it costs an hour and picks K2's chip. K1 next because
every later driver is better on MSI and `devd` is what makes the package a
package. K2 before K3: a networked box is useful headless, a silent one is
fine; the update work also needs K2 for updates over HTTPS (a USB stick
works without it). NVMe (sibling) runs in parallel with K1–K2 and benefits from K1.

## 6. Risks and open questions

1. **Unknown board.** The NIC, M.2 wiring and Wi-Fi module are guesses until
   K0. Two NIC families are named so K2 can start the day K0 lands.
2. **No QEMU model for either real NIC.** The driver is proven on the box,
   so the host-side fake device and hostile-input tests carry more weight than
   usual; CI covers the library, not the chip.
3. **Realtek revision sprawl.** The `re(4)` drivers carry per-revision PHY and
   MAC quirk tables; LazyOS supports the one revision on the box first and
   says so in the compat row.
4. **Firmware SMM and the EC.** The fan and some USB/legacy emulation run in
   firmware; the xHCI handoff (H3) already takes USB from SMM. Nothing else
   here needs a handoff *inferred*.
5. **Interrupt routing assumption.** If the board *does* program usable PIC
   lines, drivers work on INTx before K1; K1 is still wanted for efficiency
   and because MSI-X-only devices exist.
6. **Real-hardware firsts.** Every H-phase is QEMU-verified only. Expect the
   box to find a bug or two in boot before any driver here runs; K0 exists to
   find them early.

## 7. Decisions requested

1. Which mini PC is it (vendor and model), or the `devctl` output from K0?
2. Is wired Ethernet enough for v1, with Wi-Fi left to wifi-plan?
3. Is analog audio wanted on this box, or can K3 wait behind K4?
