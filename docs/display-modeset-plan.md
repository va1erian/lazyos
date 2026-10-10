# Changing the monitor resolution without a GPU driver

Investigation, not an implementation. Question: can LazyOS change what the
monitor shows (resolution) without rebuilding the kernel and without a full
GPU driver, on the two machines in [`compat/`](compat/hardware.md)?

## The two machines

| | A: Kaby Lake mini PC (`compat/kabylake/`) | B: MagicNUC AS1 (`compat/amd-nuc/hw-survey/`) |
|---|---|---|
| CPU / GPU | i3-7100U, HD 620 `8086:5916` (Gen9.5 display) | Ryzen 5 3501U, Vega 8 `1002:15d8` (Picasso/Raven2, DCN 1.0) |
| Output | HDMI-A-1, monitor lists 2560x1440 first, 1920x1200, 1920x1080... | HDMI-A-1, same monitor modes |
| Firmware | AMI 5.12, UEFI, Secure Boot **off** | AMI 2.03, UEFI, Secure Boot on in Linux; **turned off** to boot LazyOS (PR #734) |
| Registers | BAR0 `0xde000000` 16 MiB (MMIO + GTT), BAR2 `0xc0000000` 256 MiB (aperture) | BAR0 `0xe0000000` 256 MiB (VRAM window), BAR5 `0xfcb00000` 512 KiB (MMIO) |
| Boot today | GOP default 800x600; loader takes the largest GOP mode (2560x1440); kernel caps logical screen to 1080p centred | boots to the desktop since [#734](https://github.com/va1erian/lazyos/pull/734); the stock rule picked **1280x960**, fixed by the loader's largest-area choice (below) |

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

**Measured on B** (GRUB `videoinfo`, `HW:FB:` from `dbgctl log`): the GOP
lists 2560x1440 (the EDID preferred mode) *first*, then 640x480, 800x600
(the default, `*`), 1024x768, 1280x1024, 1400x1050, 1600x1200, 1280x960.
With a 1280x720 minimum the upstream "last match" rule therefore booted at
1280x960, a 4:3 mode on a 16:9 panel. The loader now takes the largest
area among the matches (`vendor/bootloader/uefi/src/mode_pick.rs`, host
tested) and logs every mode as `HW:GOP:<index>:<w>x<h>` (`*` = chosen).
The modes a GOP offers are the only ones this option can reach: on B the
smaller ones are 4:3, so 16:9 1080p/720p still needs option 2 or 3. Secure Boot on B: the loader is unsigned, so SB had to be turned off to
boot LazyOS there (done for #734); unrelated to the mode.

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
- **B (AMD DCN 1.0)**: DCHUBP surface config, DPP scaler (`SCL_*`,
  viewport, ratios, **polyphase coefficient tables must be written**), and
  MPC/OPP unchanged. Register bases come from `amdgpu`'s per-IP offsets, MMIO
  is BAR5. Much more surface than A and unproven without a dump; the DMCU
  firmware may also touch the pipe. Treat as research.

Both are "mode setting without a driver" in the sense of poking a few
registers, but they are per-GPU-generation code with no QEMU model to test
against: only the two boxes can verify them, and a wrong write blanks the
screen with no serial port to see why (use `dbgd`, docs/dbgd.md).

### 4. Real native mode set (PLL, DDI/HDMI, timings from EDID)

This is the GPU driver the question excludes: clocks, link, infoframes,
watermarks on both vendors. Not recommended.

## Recommendation

1. **Probe first, write nothing**: a read-only `hwreport`/`dbgd` addition
   that dumps the loader's GOP mode list and the display engine's registers
   (A: pipe/plane/scaler; B: DCN HUBP/DPP) as the firmware left them. That
   settles every "to confirm" above, safely, on both boxes.
2. **Ship option 2** as the portable answer to "change resolution at
   runtime": an output scale in `present`, configured by `display.*` in
   `lazyos.cfg` (no rebuild) and changed live through the compositor.
3. **Add option 1** for a true native mode on reboot (exact mode in the
   loader), if the GOP lists show the monitor modes people want.
4. **Option 3 on A only**, as an optimisation of 2 (scanout scaling for
   free) once the dump confirms the plane/scaler state; B stays on option 2.

Open questions for the maintainer: is a reboot acceptable for a native
mode change, and is a softly scaled 1080p/720p picture acceptable on a
1440p panel? If both are yes, options 1 and 2 cover both machines with no
GPU code at all.
