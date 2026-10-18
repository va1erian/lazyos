# Changing the monitor resolution without a GPU driver

> A full Intel GPU driver for the Kaby Lake box is planned in
> [kabylake-gpu-plan.md](kabylake-gpu-plan.md); it adopts the firmware mode
> and keeps the options below as its deferred M1/M2 stages.

Investigation, not an implementation. Question: can LazyOS change what the
monitor shows (resolution) without rebuilding the kernel and without a full
GPU driver, on the two machines in [`compat/`](compat/hardware.md)?

## The two machines

| | A: Kaby Lake mini PC (`compat/kabylake/`) | B: Alder Lake mini PC (`compat/alderlake/`) |
|---|---|---|
| CPU / GPU | i3-7100U, HD 620 `8086:5916` (Gen9.5 display) | i3-1220P, UHD `8086:46b3` (Alder Lake-UP3 GT1, display version 13) |
| Output | HDMI-A-1, monitor lists 2560x1440 first, 1920x1200, 1920x1080... | HDMI-A-1 at 2560x1440 (the same monitor model); DP-1, DP-2, HDMI-A-2 and HDMI-A-3 disconnected |
| Firmware | AMI 5.12, UEFI, Secure Boot **off** | AMI `DB19C0.19`, UEFI, Secure Boot **off** |
| Registers | BAR0 `0xde000000` 16 MiB (MMIO + GTT), BAR2 `0xc0000000` 256 MiB (aperture) | BAR0 `0x6000000000` 16 MiB (MMIO), BAR2 `0x4000000000` 256 MiB (aperture) |
| Boot today | GOP default 800x600; loader takes the largest GOP mode (2560x1440); kernel caps logical screen to 1080p centred | **not booted yet** (Linux survey only): the GOP mode list and the firmware's register state are *to confirm* |

Note: the two folders under `compat/kabylake/` (`hw-survey`, `grub-survey`)
are the same box surveyed twice (same board, CPU, GPU, monitor).

## What exists today

- `display.mode=WxH` in `lazyos.cfg` (read at boot from the FAT, **no kernel
  rebuild**) only drives QEMU's Bochs adapter (`kernel/src/display/bochs.rs`).
  On real hardware it is refused (`no Bochs DISPI adapter`).
- `display.max=WxH` and `display.scale` also need no rebuild, but they only
  change the *logical* screen inside the firmware's mode: the monitor stays
  at 2560x1440 with a black border (Kaby Lake `HW:FB:2560x1440->1920x1080`).
- The mode itself is chosen by the `bootloader` 0.11 UEFI stage (now vendored at `vendor/bootloader` by #734) through GOP
  `SetMode` before `ExitBootServices`
  (`bootloader-x86_64-uefi/src/main.rs:init_logger`): it takes the **last**
  GOP mode with width and height >= the minimum. The minimum comes from
  `boot.json` on the ESP root (read at every boot, overrides the kernel's
  embedded value) or the kernel's embedded config. It cannot ask for an
  exact mode, so a minimum below the largest mode still yields the largest.

## Options

### 1. Pick the GOP mode in the loader (reboot to change)

The firmware's own display driver does the mode set; LazyOS contains no GPU
code. Needs: an *exact/preferred* mode in the loader (a small patch of the
UEFI stage, which #734 already vendors and patches: a loader rebuild, not a
kernel rebuild) fed from `boot.json` or an EFI variable that the OS writes.
Cannot change resolution in a running session (GOP is gone after
`ExitBootServices`; there is no runtime-services equivalent).

**Measured on the retired AMD NUC** (GRUB `videoinfo`, `HW:FB:` from
`dbgctl log`): the GOP lists 2560x1440 (the EDID preferred mode) *first*, then
640x480, 800x600 (the default, `*`), 1024x768, 1280x1024, 1400x1050, 1600x1200,
1280x960. With a 1280x720 minimum the upstream "last match" rule therefore
booted at 1280x960, a 4:3 mode on a 16:9 panel. The loader now takes the
largest area among the matches (`vendor/bootloader/uefi/src/mode_pick.rs`,
host tested) and logs every mode as `HW:GOP:<index>:<w>x<h>` (`*` = chosen);
[`compat/hardware.md`](compat/hardware.md) "Retired machines" has the record.
The modes a GOP offers are the only ones this option can reach: on the retired
box the smaller ones were 4:3, so 16:9 1080p/720p still needs option 2 or 3;
neither current box's GOP list has been captured yet (*to confirm*). The
loader is unsigned, so Secure Boot had to be turned off on the retired box to
boot LazyOS (#734) and is off on both current boxes; unrelated to the mode.

### 2. Scale in software at `present` (works everywhere, live)

Keep the scanout at the native mode and let `display/present.rs` expand the
logical screen to it: exact 2x pixel doubling for 1280x720 on 2560x1440,
bilinear (or integer-ratio-biased) for 1080p-on-1440p. Needs no hardware
knowledge, so identical on A, B, QEMU and any GOP machine. Costs CPU and
write-combined bandwidth per dirty rectangle (A is already reported laggy,
`compat/kabylake/boot-1/NOTES.md`), so it must stay dirty-rect based. A live
switch also needs `xuid` to re-create its buffers: `display::switch_to`
refuses once the compositor has bound the display, and any option that
changes the logical size at runtime meets this, the hardware ones too.

### 3. Re-program only the plane and scaler (no PLL, no link)

The firmware already trained the link and programmed the timings; a mode
"switch" that keeps the output timing at 2560x1440 only changes what is
read from memory and how it is scaled. No clocks, no EDID, no DDI.

- **A (Intel Gen9)**: pipe A plane 1 size/stride/surface registers, `PIPESRC`
  and the pipe scaler (`PS_CTRL/WIN_POS/WIN_SZ`), roughly ten MMIO writes,
  armed by writing `PLANE_SURF`. The surface already sits in the GTT (the
  smaller mode fits in the 2560x1440 buffer). Risks: FIFO underrun if the
  firmware's watermarks/DDB do not cover the scaler, plane format limits.
  Offsets are from memory of Linux `i915`; **to confirm** against a dump.
- **B (Intel display 13, Alder Lake)**: the same idea, but not the same
  registers as Gen9; nothing is written for it and its firmware state is not
  captured yet. It stays on option 2 until a dump exists. (The retired AMD
  NUC's DCN 1.0 path went with the machine; there is no AMD box in the fleet
  now.)

These are "mode setting without a driver" in the sense of poking a few
registers, but they are per-GPU-generation code with no QEMU model to test
against: only the boxes can verify them, and a wrong write blanks the screen
with no serial port to see why (use `dbgd`, docs/dbgd.md).

### 4. Real native mode set (PLL, DDI/HDMI, timings from EDID)

This is the GPU driver the question excludes: clocks, link, infoframes,
watermarks. Not recommended.

## Recommendation

1. **Probe first, write nothing**: a read-only `hwreport`/`dbgd` addition
   that dumps the loader's GOP mode list and the display engine's registers
   (Kaby Lake: pipe/plane/scaler; the Alder Lake box: the same kind of dump,
   once it boots) as the firmware left them. That settles every "to confirm"
   above, safely, on both boxes.
2. **Ship option 2** as the portable answer to "change resolution at
   runtime": an output scale in `present`, configured by `display.*` in
   `lazyos.cfg` (no rebuild) and changed live through the compositor.
3. **Add option 1** for a true native mode on reboot (exact mode in the
   loader), if the GOP lists show the monitor modes people want.
4. **Option 3 on the Kaby Lake box only**, as an optimisation of 2 (scanout
   scaling for free) once the dump confirms the plane/scaler state; the
   Alder Lake box stays on option 2.

Open questions for the maintainer: is a reboot acceptable for a native
mode change, and is a softly scaled 1080p/720p picture acceptable on a
1440p panel? If both are yes, options 1 and 2 cover both machines with no
GPU code at all.
