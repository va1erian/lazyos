# Plan: USB boot and the minimal feature set for a real PC

> **Status: draft proposal, revision 1 (2026-10-01).** Builds on
> [`architecture/boot.md`](architecture/boot.md) (the image pipeline),
> [`architecture/block-devices.md`](architecture/block-devices.md) (the ramdisk
> fallback from issue #5), [`driver-plan.md`](driver-plan.md) (the device core),
> [`usb-hid-plan.md`](usb-hid-plan.md) (USB keyboards and mice, QEMU only) and
> the S9 "release images" line of [`platform-plan.md`](platform-plan.md).
> No earlier plan or tracking issue covers booting outside QEMU; the USB HID
> plan names "real-hardware BIOS handoff" as an explicit non-goal, and
> `build.rs` only emits a BIOS image today (the README's old "boots under
> UEFI" was aspirational and now says so). This plan is the missing piece.

> **H0 status:** landed as one hybrid MBR image instead of two images and a
> FAT16 payload: `target/lazyos-usb.img` (`LAZYOS_USB_IMAGE=1`) boots under
> OVMF and SeaBIOS from `usb-storage` only, its ramdisk is a whole disk (FAT
> `lazyos.cfg` plus the ext2 OS volume) and `ram0` wins root selection; its
> `lazyhome` partition is `/home`, mounted late through `usbd` and kept across
> power-offs (`tools/boot/persist.py`); see
> [`usb-stick.md`](usb-stick.md) for the layout, the measured sizes and boot
> times, and `tools/boot/`.

## Goal and scope

Write one image to a USB stick, plug it into an ordinary x86_64 PC built
between roughly 2012 and 2025, and reach the LazyOS desktop with a working
keyboard and mouse, in either firmware mode (UEFI or legacy BIOS/CSM). When
that fails, the machine must say *why* on its own screen, because real PCs have
no serial port.

**The minimal feature set** ("boots on a real PC" means all of these):

1. **Boot media:** a BIOS image and a UEFI image from the same build, both
   written with `dd`/Etcher/Rufus; later one hybrid image.
2. **Root volume without a disk driver:** every file the build embeds is
   reachable from a RAM-resident volume, because the kernel's only storage
   drivers are ATA PIO (primary master) and legacy virtio-blk, and a USB stick
   is neither.
3. **Survive real firmware:** fragmented memory maps, 50 PCI functions, a
   floating IDE bus, no i8042, no COM1, a 4K panel, 32 GiB of RAM.
4. **A timer that ticks:** the scheduler runs on the 8254 PIT at 100 Hz; Intel
   platforms since about 2019 ship with the PIT clock-gated by default.
5. **Input:** PS/2 where it exists (most laptops), USB HID where it does not
   (most desktops in native UEFI mode), via the USB HID plan plus the
   real-hardware deltas listed here.
6. **Shutdown and reboot** that work off QEMU's magic ports.
7. **Evidence without serial:** on-screen panics, a boot log you can read and
   photograph, a hardware report, and a compatibility matrix.

**Out (v1):** persistence to the stick or to internal disks (AHCI, NVMe, USB
mass storage; [nvme-install-plan.md](nvme-install-plan.md) explores installing on an internal NVMe SSD), networking and sound on real controllers (e1000e, Intel HDA),
SMP (S8), ACPI power management beyond S5, Secure Boot signing, laptop
specifics (I2C-HID touchpads, backlight, lid), Wi-Fi ([wifi-plan.md](wifi-plan.md)
explores it, with the M.2 card as its last stage), GPU drivers. Each has a
seam named in the phases; none is needed to boot.

## What exists, what is missing

| Piece | State today | Needed |
|---|---|---|
| Image builder | `bootloader` 0.11.17 `DiskImageBuilder`; `build.rs` calls only `create_bios_image` (MBR + FAT, INT 13h LBA loads). The same builder offers `create_uefi_image` (GPT + ESP with `EFI/BOOT/BOOTX64.EFI`) | emit both; later a hybrid image |
| Kernel reads its files | from the boot FAT partition through the ATA driver | a RAM-resident copy of the same file set |
| Ramdisk (#5) | `LAZYOS_RAMDISK=<fat image>` is loaded by the bootloader and registered as `ram0` *after* ATA/virtio; `fs::init` falls through to it; `Fat16::open` accepts a bare image | build the ramdisk from the build's own file table instead of an external file; make `ram0` the normal root on real hardware |
| FAT | FAT12/16 read-only, long names (`fs/fat/lfn.rs`); FAT32 refused (#248) | force FAT16 when formatting the payload volume so a large image never silently becomes FAT32 |
| Memory map | `mem::init` keeps at most `MAX_REGIONS = 32` usable regions and silently `break`s past that | coalesce adjacent regions, raise the cap, log the total |
| PCI | config mechanism 1, bridge walk, `MAX_DEVICES = 32`; overflow prints `DEV:ENUM:FAIL:device table full` | a table sized for a laptop (64+ functions) or a counted overflow that still attaches drivers |
| ATA probe | polled `wait_not_busy`/`wait_for_data` loops bounded at 1 M reads; a status of 0 bails early | treat a status of `0xFF` (floating bus, no IDE controller in AHCI mode) as "absent" before any wait; see #449 |
| i8042 | `mouse::init` waits are bounded (100 k polls) so a missing controller cannot hang boot; no controller self-test | detect absence once, skip mouse init, log it |
| Serial | `uart_16550` on COM1 unconditionally; an absent port reads `0xFF`, so `send` does not block | detect absence (scratch-register test), keep logging into a RAM ring regardless |
| Framebuffer | bootloader GOP/VBE mode, `Rgb`/`Bgr`, stride honoured (`gfx.rs`); the display grant's `bind` allocates one screen-sized RGBA shared buffer (`width * height * 4`, `display.rs`) under the per-process shared-buffer allowance, which `kernel/src/limits.rs` now derives from the screen (three surfaces, at least 16 MiB), and the kernel heap grows on demand (docs/architecture/limits.md), so a 4K mode (31.6 MiB per surface) binds given the RAM; it used to be an 8 MiB cap and a fixed 16 MiB heap, on which a 4K `bind` failed. `BootConfig.frame_buffer` (via `DiskImageBuilder::set_boot_config`; the `BootloaderConfig.frame_buffer` field is deprecated since 0.11.1) sets only *minimum* width and height; 0.11.17 has no maximum, so the image cannot cap the mode | a kernel-side **logical screen**: at most 1920x1080 (7.9 MiB RGBA, inside both budgets) centred in whatever mode firmware chose, the rest black; every screen buffer sized from the logical screen, not the mode; the chosen mode checked against the budgets at boot with the verdict on screen; a bootloader maximum-mode patch as a later improvement; measure uncached writes |
| Timer | PIT channel 0 at 100 Hz, PIC only | local APIC timer calibrated from ACPI, PIT kept for QEMU and old boards |
| ACPI | not parsed; `BootInfo.rsdp_addr` is available from the bootloader | a small hostile-input table walker (RSDP, XSDT, FADT, MADT, HPET) |
| Power | reboot via 8042 `0xFE`; shutdown writes QEMU's `0x604`/`0xB004` | FADT reset register, ACPI S5 via PM1a control and `\_S5` |
| Panic | `serial_println!` then halt; nothing on screen | render the panic and the last log lines on the framebuffer |
| Input on legacy-free PCs | PS/2 only; USB HID is planned for QEMU `qemu-xhci` | the HID plan plus BIOS handoff, hubs, port timing |
| Test rig | `qemu_shot.py`/`qemu_session.py` boot a raw image on IDE; no OVMF, no USB media | `--firmware uefi`, `--media usb`, `--no-serial`, `--no-i8042` variants and a CI matrix |

## Key decisions

1. **The root volume is a ramdisk on real hardware, by design, not as a
   fallback.** A USB stick is only reachable through firmware (INT 13h or
   the EFI file protocol), which the bootloader uses and the kernel does
   not. Writing a USB mass-storage or AHCI driver just to read the files back
   would put the whole USB stack on the boot path. Instead `build.rs` formats
   the embedded file set into a FAT16 **payload volume**, hands it to the
   bootloader as the ramdisk, and the kernel applies one **root-selection
   rule**: *a bootloader ramdisk, when present, is the boot volume.* `fs::init`
   mounts `ram0` at `/` before any disk is probed for a root, and disks are then
   probed only for the ext2 `/data` volume. Today's first-openable-volume order
   (`block-devices.md`) applies only when no ramdisk was handed over or the
   payload fails to mount, so a PC whose internal disk happens to carry a FAT or
   ext2 volume the kernel can open still boots the stick's files, never the
   disk's. The payload rides in **every** image, so QEMU CI exercises exactly
   the root path a real PC takes; H0 measures the boot-time cost and only if it
   is too high for the CI matrix does the QEMU image drop the payload behind a
   build switch while the stick images keep it. A run mode boots the image
   through firmware from an emulated USB stick, with no IDE or virtio disk at
   all, to prove the RAM path end to end. The boot partition and the ramdisk
   carry the **same files** from one table, so there is one source of truth.
2. **UEFI is the primary target, BIOS the second, one hybrid image the goal.**
   Many 2020+ machines have no CSM. The `bootloader` crate builds either image
   from the same kernel, so v1 ships `target/lazyos-uefi.img` and the existing
   `target/lazyos.img` (BIOS). A hybrid (protective MBR with the BIOS stage-1,
   GPT, one ESP that is also the BIOS FAT partition) is a host-side image
   assembler (`tools/image/`, Python like `tools/mkdisk/`) that the crate does
   not provide; it lands once both single-mode images are proven.
3. **Hostile firmware is an input.** Memory maps, PCI config space, ACPI
   tables and USB descriptors come from vendors, not from QEMU. Every parser
   is a pure `no_std` library with host tests and seeded fuzz entries
   (`fuzz::run(&[u8])`, checked-in seeds, `fuzz/gen_corpus.py --check`), every
   loop is bounded, and a table that fails validation disables the feature
   that needed it rather than the boot.
4. **Legacy hardware stays the QEMU path; detection picks the real one.** The
   PIT, 8259, i8042 and COM1 keep working unchanged under QEMU. On real
   hardware the kernel probes each once, prints a `HW:<unit>:PRESENT|ABSENT`
   line, and switches to the alternative (LAPIC timer, USB HID, RAM log).
   No build switch selects "real hardware"; one image boots everywhere.
5. **No new kernel knowledge of USB.** Driver-plan D1/D2 hold: `usbd` stays a
   userspace driver; the real-hardware additions (handoff, hubs, timing) are
   `usbd` and `libs/xhci` work, not kernel work.
6. **Evidence is a screen, a photo and a report.** Without serial, the
   verdict comes from the framebuffer: boot markers and panics render there,
   a boot-log ring is readable from the Terminal (`dmesg`), and a `hwreport`
   command writes the machine's inventory as text the tester copies into
   `docs/compat/hardware.md`. CI still judges the serial trace in QEMU, as
   every other harness does.

## Architecture

```
 build.rs ─┬─ kernel.trimmed ──────────────┐
           ├─ file table (ELFs, docs, …) ──┼─ boot FAT partition (QEMU: ATA/virtio)
           │                               └─ payload.fat16 ──► bootloader ramdisk
           ├─ create_bios_image ──► target/lazyos.img       (MBR, INT 13h)
           └─ create_uefi_image ──► target/lazyos-uefi.img  (GPT + ESP)
                      later: tools/image/hybrid.py ──► target/lazyos-usb.img

 boot: firmware ─► bootloader (GOP/VBE mode, memory map, RSDP, ramdisk) ─► kernel
   kernel: serial? ─► console ─► mem (coalesced map) ─► dev (PCI, bounded ATA probe)
           ─► fs (ramdisk handed over: ram0 = /, disks for /data only;
                  else today's disk order) ─► arch (PIC/PIT, or LAPIC timer
           from ACPI) ─► i8042? ─► services ─► usbd (HID) ─► xuid desktop
   evidence: HW:* lines ─► serial if present, always the RAM log ring
             panic ─► framebuffer; `dmesg`, `hwreport` in the Terminal
```

## Phases

Each is independently shippable. Every kernel-facing phase ships correctness
**and** stress tests under `kernel/src/tests/` and must pass
`python tools/test/run.py --accel none` (AGENTS.md). Every phase adds its QEMU
variant to the new harness (below) so a regression is caught before anyone
walks to a real machine. Source files stay under 500 lines; `build.rs` is
already near 400, so the image work extracts modules under `build_support/`.

| Phase | Deliverable | Tests and evidence |
|---|---|---|
| **H0** Boot media and the RAM root | `build_support/payload.rs`: one file table feeding both the boot partition and a FAT16 payload volume (formatted with the FAT type forced, 8.3 names plus long names exactly as today); `set_ramdisk` of that volume unless `LAZYOS_RAMDISK` overrides it; `create_uefi_image` alongside the BIOS image; the root-selection rule from decision 1 in `fs::init` (ramdisk present means `ram0` is `/`, disks are probed for `/data` only; the old order is the fallback), which logs `FS:ROOT:<device>`; the kernel prints `BOOT:MEDIA:<bios\|uefi>` from `BootInfo`. Size budget and boot time measured and recorded in `architecture/boot.md` (the ramdisk doubles the on-disk payload; INT 13h loads it in real mode, so BIOS boot time is the number to watch; `block-devices.md` is updated to describe the rule). README corrected until UEFI really boots | `ramdisk_suite` grows: payload mounts, long names resolve, FAT type is 16 regardless of size, a corrupt payload is refused cleanly and falls back to the disk order, and the selection rule picks `ram0` over a fake disk device that carries an openable volume. Harness: SeaBIOS and OVMF each booting from `usb-storage` *only* (no IDE, no virtio); the same images with an IDE disk attached that carries a recognised FAT volume (the current QEMU boot partition) and with a virtio ext2 data disk, where `FS:ROOT:ram0` is still required and `/data` still mounts; desktop screenshot judged by `pngstats.py` and read |
| **H1** Firmware robustness | `mem::init` coalesces adjacent usable regions, raises `MAX_REGIONS`, logs total and dropped RAM; PCI table sized for real machines (or a counted overflow that keeps attaching drivers); `ata::probe` rejects a `0xFF` status before waiting (fixes the shape of #449); i8042 presence probe gating `mouse::init`; COM1 presence probe; a boot-log ring (`klog`) that every `serial_println!` also feeds, read by a new syscall op and surfaced as `dmesg` in `sh`; the panic handler and `kstop` draw the message and the last ring lines on the framebuffer; the **logical screen** in `display.rs`: the display grant exposes `min(mode, 1920x1080)` centred in the framebuffer (`present` offsets the blit, the console keeps the full mode), so a 4K panel runs the desktop at 1080p inside its budgets instead of failing `bind`; every screen buffer (display grant, compositor, xui clients via the geometry they are told) sized from the logical screen; at boot the kernel checks the chosen mode against the shared-buffer cap and the heap and prints `HW:FB:<mode>-><logical>`, with a mode it still cannot carry reported on screen instead of a silent halt; `BootConfig.frame_buffer` minimums only guard against a tiny firmware default (640x480 text-era modes), never as a cap; `hwreport` (CPU brand and features, RAM, memory-map summary, PCI list with classes, framebuffer mode, `HW:*` verdicts) | `mem_suite`: synthetic fragmented maps (UEFI shape, 60 regions, holes, regions above 4 GiB) and a soak over many maps; `dev_suite`: a full table still attaches the block drivers; a fake ATA device answering `0xFF`; `klog` ring wrap and concurrent writers; `display_suite`: a 3840x2160 mode yields a 1920x1080 logical screen centred with the right offsets and a `bind` buffer under the cap, a 640x480 mode is used as is, a 2560x1600 mode is reduced per axis to 1920x1080 with the borders uneven but correct, and `present` never writes outside the framebuffer at any offset. Harness variants: `-m 8G`, `-serial none` (must not hang, must still show the desktop), `-machine q35` with no IDE, `i8042=off` where the QEMU build supports it, `-device VGA` modes of 640x480 and 3840x2160 (the 4K run must show the desktop centred with black borders, judged by `pngstats.py` on the border and centre regions), forced panic rendering screenshot |
| **H2** Timekeeping without the PIT | `libs/acpi`: RSDP (from `BootInfo.rsdp_addr`), XSDT/RSDT, FADT (PM timer, reset register, PM1a control, century register), MADT (LAPIC address, IOAPIC entries recorded for S8), HPET; checksums and lengths validated. Local APIC enabled with the 8259 kept in virtual-wire mode (LINT0 ExtINT) so IRQ1/IRQ12 and the PCI lines keep arriving; LAPIC timer calibrated against the ACPI PM timer (CPUID leaf 0x15/0x16 TSC as the second source) and used as the 100 Hz tick when the PIT is absent or found not counting; PIT stays the default where it works. The tick source is one `HW:TIMER:<pit\|lapic>` line | `cargo test -p acpi` with golden tables from QEMU `pc`/`q35` and OVMF, plus seeded fuzz; kernel `timer_suite`: the chosen tick matches the CMOS RTC over several seconds within tolerance on both sources, PIT-dead detection does not false-trigger under load, deadlines and CPU accounting (`task::ticks`) unchanged; interaction with #344 measured. Harness: a run forcing the LAPIC path, `-machine q35` and `pc`, `-no-hpet` |
| **H3** Input on legacy-free machines | On top of usb-hid-plan U2/U3: xHCI BIOS-to-OS handoff (USBLEGSUP extended capability, SMI disable) before any register write; port power, reset and debounce timing per spec rather than QEMU's tolerance; **hub class** (external ports and front panels sit behind hubs; the HID plan excludes hubs); interrupt-IN via INTx or polling as the HID plan decides; `usbd` starts only when `HW:I8042:ABSENT` or always (decision below). PS/2 keyboard and touchpad paths unchanged | `tools/usb/run.py --hub` with QEMU `usb-hub`; the HID plan's no-PS/2 run mode becomes the real-hardware rehearsal; handoff code unit-tested against a fake capability list. Real machines: the compatibility matrix rows for "USB keyboard at the firmware prompt works in LazyOS" |
| **H4** Power and the long tail | Reboot order: FADT reset register, 8042 `0xFE`, triple fault; shutdown: ACPI S5 (`\_S5` package found by a bounded byte scan of the DSDT, PM1a/PM1b control from the FADT), QEMU ports kept as the last resort, then an on-screen "safe to power off". Seams documented, not built: USB mass storage for a persistent `/data` on the stick (needs a userspace block provider interface in `idl/`, since `usbd` is userspace and block drivers are kernel), AHCI/NVMe for internal disks, IOAPIC/MSI for S8 | `power` suite: refusal paths unchanged, the S5 scan rejects hostile DSDTs; harness: `shutdown` under OVMF ends the VM, `reboot` restarts it |

**H3 status (implemented, QEMU-verified where QEMU can model it):** `usbd`
drives every xHCI controller, takes each from the BIOS (USB Legacy Support
semaphore, bounded wait, SMIs off) before any other register write, uses
64-byte contexts when `CSZ` says so, reads the Supported Protocol
capabilities to treat USB 2 and USB 3 ports each their own way (debounce,
reset and recovery; link training and warm reset), handles low-, full-,
high- and SuperSpeed devices (endpoint 0 size, interval encoding, ESIT
payload), drives USB 2 and SuperSpeed hubs (route string, TT, multi-TT,
five tiers) and binds every boot interface of a composite device. Verified
by `tools/usb/run.py --hub`, `--full-speed` and `--controllers 2`, and by
host tests for what QEMU lacks (64-byte contexts, the handoff against a
model BIOS, high-speed and SuperSpeed hubs). See
[`architecture/usb.md`](architecture/usb.md). Interrupts stay polled.

### Open decision: when does `usbd` run?

Always running `usbd` is simplest and matches how desktops behave, but on a
machine with SMM legacy USB emulation, both the firmware (through the i8042)
and `usbd` would deliver the same keystrokes until the handoff completes. The
handoff in H3 is therefore unconditional and runs before `usbd` touches a port;
after it, the i8042 emulation stops by itself. Decide with the HID plan's owner
whether `init` starts `usbd` on every boot or only when the i8042 probe fails.

## Verification harness (`tools/boot/`)

Modelled on `tools/sound/run.py` and `tools/net/run.py`: the verdict is pixels
and markers, not source reading.

- `run.py --firmware bios|uefi --media ide|virtio|usb [--no-serial] [--no-i8042]
  [--memory 8G] [--fb WxH] [--timer lapic]` builds (or `--no-build`), boots
  headless (OVMF from the distribution package for `uefi`; `-device qemu-xhci
  -device usb-storage,drive=stick` for `usb`), captures screenshots through
  `qemu_qmp.py`, and judges: `BOOT:MEDIA`, `FS:ROOT`, `HW:*` lines where serial
  exists, `pngstats.py` thresholds always, and a typed command echoing in the
  Terminal when input is part of the run.
- `write_stick.py`: lists removable block devices only, refuses anything that
  is not removable or is mounted, shows size and model, asks twice, then writes
  and verifies by reading back. Linux and Windows (`\\.\PhysicalDriveN`).
- `test_run.py`: the judge fails when it should (missing `FS:ROOT:ram0`, black
  screen, wrong firmware marker).
- `docs/compat/hardware.md`: one row per machine (vendor, model, year,
  firmware mode, CPU, RAM, GPU/framebuffer mode, `HW:*` verdicts, input that
  worked, what failed, photo link), filled from `hwreport` output by whoever
  ran the stick. The ABI matrix is the model: honest rows beat a claim.
- CI `.github/workflows/boot.yml`: the matrix `{bios, uefi} x {ide, usb}` plus
  the H1 variants, artifacts published like `screenshots.yml`. Real-hardware
  rows are manual by nature and the matrix says so.

## Risks and open questions

1. **BIOS load time of a large ramdisk.** INT 13h in real mode moves data in
   small chunks; a payload of tens of MiB may take long enough to look hung.
   H0 measures it and, if needed, trims the stick profile (no docs embedding,
   `LAZYOS_DESKTOP` only) or shows progress from the stage-2 loader. The UEFI
   path has no such cost.
2. **Uncached framebuffer writes.** The bootloader's framebuffer mapping and
   the firmware's MTRRs decide whether the present blit runs at write-combining
   or uncached speed; on a 4K panel the difference is a usable or an unusable
   desktop. H1 measures and, if needed, remaps the framebuffer write-combining
   through PAT in `mem::init` (done in H1: `mem::wc`, logged as `HW:FB:WC:`; bare metal only: under a hypervisor the framebuffer is guest RAM, and a host that honours the guest PAT (KVM on AMD) would take every present out of the cache). The logical-screen cap (H1) bounds the blit
   size; a bootloader-side maximum mode is the third lever.
3. **Virtual-wire mode is an assumption.** H2 enables the LAPIC while keeping
   the 8259 for device interrupts. Firmware normally leaves LINT0 as ExtINT;
   a board that does not would lose keyboard and PCI interrupts the moment the
   LAPIC is enabled. The fallback is to route through the IOAPIC, which the
   MADT already describes and S8 needs anyway; H2 keeps the IOAPIC table and
   decides on first real-machine evidence.
4. **Dead PIT detection.** A PIT that counts but is slow, or a firmware that
   gates it after boot, must not be misread. H2 cross-checks against the PM
   timer rather than trusting one read.
5. **Firmware double-delivers USB keys** until the xHCI handoff lands (H3);
   until then a USB keyboard works on CSM machines through the i8042 emulation
   and not at all in native UEFI, and the matrix says which.
6. **Secure Boot** must be off; the kernel is unsigned. Document it on the
   first page of the stick instructions. Signing is a later S9 item.
7. **Memory above 4 GiB and 64-bit BARs** are handled by the frame allocator
   and `pci.rs`, but no run has exercised a 32 GiB map or a BAR at 0x4000_0000_0000;
   H1's `-m 8G` and q35 PCIe variants are the rehearsal.
8. **`bootloader` crate limits.** `BootConfig.frame_buffer` can only raise
   the mode (minimum width and height); 0.11.17 has no maximum, so a 4K panel
   stays 4K and the kernel must cope (H1's logical screen). There is no boot
   menu, and the UEFI stage exits boot services before the kernel runs (no
   runtime services for us). If a limit bites, the choice is to patch the
   crate (it is Rust, upstreamable; a `maximum_framebuffer_*` pair is the
   obvious first patch) rather than write a loader; this plan does not budget
   for a loader.
9. **Vendor quirks have no QEMU model.** Everything above lowers the odds; the
   matrix and photos are how the odds are measured. Expect the first three
   machines to each find one new bug, and the fourth to boot. The PC will be
   fine; it is the plan that gets rebooted.
10. **Persistence expectations.** A stick that boots a desktop invites "where
    did my file go?" `/tmp` is RAM and the root is read-only; the Files app
    and the Terminal should say so until H4's follow-up gives `/data` a home.

## Suggested order of work

H0 first: it is the smallest change (a payload volume and a second image) and
the only one that can be proven today on a real machine with no kernel risk.
H1 next, since each item is a one-line failure on some real PC and all are
testable under QEMU. H2 is the one piece of new kernel infrastructure and
should land with `libs/acpi` fuzzed before it touches a register. H3 waits for
the HID plan's U2 and adds only the real-hardware deltas. H4 closes the loop.
A plausible first PR is H0 plus `tools/boot/run.py` with the `{bios, uefi} x
{ide, usb}` matrix, since that alone turns "boots under UEFI" from a README
sentence into a CI row.
