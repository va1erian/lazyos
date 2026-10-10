# AMD NUC (Ryzen 5 3501U, Raven2): collected facts

A MAGICNUC AS1 mini PC (board ADB19D, AMI BIOS 2.03 of 2026-04-13), UEFI.
`hw-survey/` is `tools/boot/survey_linux_box.sh` run on its Ubuntu 26.04
(redacted by `tools/boot/redact_survey.py`).

The machine is genuine: CPUID family 17h model 18h matches DMI's "Ryzen 5
3501U"; the one 8 GB DDR4-2667 DIMM shows as 4.9 GB to Linux because the BIOS
gives 3 GB to the Vega GPU (`Detected VRAM RAM=3072M`; the e820 map tops out at
8.5 GB).

| Function | Id | LazyOS |
|---|---|---|
| SATA (AMD FCH, AHCI) | `1022:7901` | `ahci` by class |
| 2x Ethernet, RTL8168h XID 541 | `10ec:8168` | `rtl8168` (same chip as the Kaby Lake box) |
| 2x xHCI | `1022:15e0`, `15e1` | `usbd` by class |
| HDA, HDMI (ATI R6xx) at 04:00.1 | `1002:15de` | `sndd` by class; no analog |
| HDA, Conexant SN6140 at 04:00.6 | `1022:15e3` | `sndd` by class; **to confirm** which controller `devd` picks |
| Vega GPU | `1002:15d8` | UEFI GOP framebuffer only |
| RTL8822CE Wi-Fi, Realtek BT | `10ec:c822` | out of scope ([`../../wifi-plan.md`](../../wifi-plan.md)) |

## First boot: reset right after the loader's last line (fixed)

The stick printed the loader log up to `Jumping to kernel entry point at ...`,
went black with a flicker and restarted after about 5 s, in a loop. Cause:
`bootloader` 0.11.17's UEFI stage never disables interrupts after
`ExitBootServices`; it replaces the GDT and page tables and jumps to a kernel
that has no IDT yet, so any interrupt the firmware's devices raise in that
window triple-faults. Intel boxes and OVMF never raise one there; this
board does.

A/B on the box, same kernel, same stick image apart from the loader: the stock
loader reset every time (after a few early-boot markers), the loader with a
`cli` after `ExitBootServices` booted to the desktop. Fix:

- `vendor/bootloader`: the 0.11.17 sources, with the one-line `cli` in
  `uefi/src/main.rs`, wired in with `[patch.crates-io]` (its `build.rs` builds
  `uefi/` when that directory exists). Remove it when upstream disables
  interrupts itself.
- `kernel_main` also disables interrupts first, so the kernel does not depend
  on how its loader hands over.

## Display: 1280x960 on a 2560x1440 panel (fixed)

GRUB's `videoinfo` shows the AMD GOP lists 2560x1440 (the EDID preferred
mode) first and eight modes in all: 640x480, 800x600 (firmware default),
1024x768, 1280x1024, 1400x1050, 1600x1200, 1280x960 follow. The stick asks
for at least 1280x720 and `bootloader` 0.11.17 takes the *last* match, so
the kernel reported `framebuffer 1280x960` (read over `dbgd`). The vendored
loader now takes the largest area among the matches and logs each mode as
`HW:GOP:<index>:<w>x<h>`; see [`../../display-modeset-plan.md`](../../display-modeset-plan.md).
Only the modes the GOP lists are reachable without a GPU driver.

## Still to check on the box

- Which HDA controller `devd` binds (the HDMI one has no analog codec).
- AHCI on the FCH, both RTL8168 ports, xHCI behaviour, memory above 4 GiB.
