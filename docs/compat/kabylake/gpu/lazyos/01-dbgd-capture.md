# Kaby Lake box under LazyOS: what the GOP leaves (2026-10-19)

The LazyOS half of stage G0 ([`../../../../kabylake-gpu-plan.md`](../../../../kabylake-gpu-plan.md)
section 9): the HD 620 (`8086:5916`) read over `dbgd` on a stick built with
`--desktop --dbgd-control --dbgd-mmio --usb-image` (no `--gen9gfx`, so no
driver claims `00:02.0` and nothing has written a display register since the
UEFI GOP). Beside this file: `02-mmio-gop.txt` (the display ranges of
`libs/pciprobe/src/builtin.rs` and the forcewake acks, zeros left out) and
`03-ggtt-scanout.txt` (the first 3616 GGTT entries). Compare with i915's
state in `../linux/` and the plan's section 2.1.

## PCI configuration (`00:02.0`, rev `0x02`)

| Field | Value |
|---|---|
| BAR0 (GTTMMADR) | 64-bit, `0xde000000` (16 MiB) |
| BAR2 (GMADR) | 64-bit prefetchable, `0xc0000000` (256 MiB) |
| `GMCH_CTRL` (`0x50`) | `0x01c1`: GMS 1 = **32 MiB** stolen, GGMS 3 = 8 MiB GGTT |
| `BDSM` (`0x5C`) | `0xbe000001`: stolen memory at `0xbe000000`, locked |
| `ASLS` (`0xFC`) | `0xbcfb1018` (the OpRegion) |
| Subsystem | `8086:2212`; pin INTA, line 11 |

Host bridge `8086:5904`: `MCHBAR` `0xfed10001`, `TOLUD` (`0xBC`) `0xc0000001`,
`BDSM` (`0xB0`) `0xbe000001`, `BGSM` (`0xB4`) `0xbd800001`, `TSEG` (`0xB8`)
`0xbd400001`.

## The firmware's display path

pipe A -> transcoder A -> DDI B -> **HDMI**, from **DPLL1**, at 2560x1440
**59.95 Hz** (CVT reduced blanking): the same mode the Alder Lake box's GOP
sets (`../../../alderlake/gpu/lazyos/`).

| What | Register | Value | Meaning |
|---|---|---|---|
| Power wells | `HSW_PWR_WELL_CTL1` `0x45400` | `0xf0000033` | PW1 and PW2, misc I/O and DDI B I/O requested and on (`CTL2` `0x50000011`: the states) |
| DC states | `DC_STATE_EN` `0x45504` | `0` | none (no DMC needed) |
| DBUF | `DBUF_CTL` `0x45008` | `0xc000000a` | powered |
| CDCLK | `CDCLK_CTL` `0x46000`, `LCPLL1_CTL` `0x46010` | `0x0c000544`, `0xc0000000` | **675 MHz** (freq select 3, decimal 0x544), LCPLL1 enabled and locked. Linux ran 337.5 MHz |
| Timings | `HTOTAL_A`..`VSYNC_A` `0x60000`.. | `0a9f09ff 0a9f09ff 0a4f0a2f 05c8059f 05c8059f 05a705a2` | 2560/2608/2640/2720 x 1440/1443/1448/1481 |
| Pipe source | `PIPESRC_A` `0x6001C` | `0x09ff059f` | 2560x1440 |
| Scaler | `PS_CTRL_1A` `0x68180`, `PS_WIN_SZ_1A` `0x68174` | `0x90800000`, `0x0a0005a0` | **scaler 1 enabled on pipe A at 1:1** (window 2560x1440 at 0,0; `PS_HSCALE`/`PS_VSCALE` `0x8000` = 1.0). The adopter must keep or account for it; Linux used no scaler |
| Transcoder | `TRANS_DDI_FUNC_CTL_A` `0x60400` | `0x90010000` | enabled, DDI B, **HDMI** mode, 8 bpc, +HSync -VSync (Linux's 70 Hz mode: `0x90030000`, both +) |
| Infoframes | `VIDEO_DIP_CTL_A` `0x60200` | `0x00001000` | AVI only (Linux: GCP, AVI, SPD) |
| Transcoder clock | `TRANS_CLK_SEL_A` `0x46140` | `0x40000000` | DDI B |
| DDI | `DDI_BUF_CTL_B` `0x64100` | `0x80000000` | enabled; A idle (`0x81`), C to E off |
| PLL routing | `DPLL_CTRL2` `0x6C05C` | `0x00a00018` | DDI B overridden to **DPLL1**, its clock not gated (as under Linux) |
| PLL mode | `DPLL_CTRL1` `0x6C058` | `0x00000845` | DPLL1: override, **HDMI mode**; DPLL0 (CDCLK) override, link rate field 2 |
| PLL state | `LCPLL2_CTL` `0x46014`, `DPLL_STATUS` `0x6C060` | `0x80000000`, `0x00001111` | DPLL1 enabled; DPLL0 and DPLL1 locked (bits 0 and 8; bits 4 and 12 not decoded); DPLL2/3 off |
| PLL dividers | `DPLL1_CFGCR1`/`CFGCR2` `0x6C040` | `0x80800192`, `0x000002a4` | DCO 24 MHz x (402 + 16384/32768) = 9660 MHz, P=2 Q=2 K=2: **241.5 MHz** pixel clock |
| Pipe | `PIPECONF_A` `0x70008` | `0xc0000000` | enabled and active; pipes B, C and eDP off |
| Plane | `PLANE_CTL_1_A` `0x70180` | `0x84000000` | enabled, XRGB8888, linear |
| | `PLANE_STRIDE_1_A`, `PLANE_SIZE_1_A` | `0xa0`, `0x059f09ff` | 10240 bytes, 2560x1440 |
| | `PLANE_SURF_1_A` `0x7019C` | `0` | GGTT offset 0 |
| | `PLANE_BUF_CFG_1_A`, `PLANE_WM_1_A_0` | `0x01bd0000`, `0x80004005` | DDB blocks 0-445 (Linux: 0-856), level 0 WM 5 blocks |
| Audio | `AUD_CONFIG_A` `0x65000`, `AUD_PIN_ELD_CP_VLD` `0x650C0` | `0x0070fa60`, `0` | configured, **no ELD valid**: the GOP does not set up HDMI audio |
| Hotplug | `SDEISR` `0xC4000`, `SFUSE_STRAP` `0xC2014` | `0x00200000`, `6` | port B live; DDI B and C detected |
| Panel | `PP_CONTROL` `0xC7204` | `0x8` | no eDP panel powered |

59.95 Hz from the clock: 241.5 MHz / (2720 x 1481) = 59.951 Hz. Measured
from `PIPE_FRMCOUNT_A` over dbgd: 1200 frames in 20.008 s = 59.98 Hz (the
network round trips bound the error).

The scanout: GGTT entries 0..3599 are valid (flags `0x1`) and map
**contiguous** pages `0xbe000000..0xbee10000`, the first 14.06 MiB of stolen
memory; the entries after them carry on contiguously through stolen memory.
The GOP framebuffer is stolen memory seen through the aperture at `GMADR` + 0.

## What this settles for G1

- The firmware's link mode is HDMI (not DVI), 8 bpc, on DDI B from DPLL1:
  the same routing i915 used, so `adopt` reads one path on both.
- CDCLK is 675 MHz under the GOP: twice what Linux ran, comfortably above any
  1440p pixel clock. The "refuse a mode CDCLK cannot carry" rule holds as
  written.
- **A pipe scaler is in the path at 1:1.** An adopter that rewrites the plane
  only must leave `PS_CTRL_1A` alone (or switch it off as part of a modeset).
- Infoframes: AVI only, no GCP; audio: no ELD. HDMI audio (G2) has to program
  both itself.

## Reading it again

```
python tools/run_demo.py --desktop --dbgd-control --dbgd-mmio --usb-image --build-only
python tools/dbg/dbgctl.py --host <box> pci-config 00:02.0 0 256
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x45400 16
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x60000 0x500
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x6C040 0x24
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x70000 0x400
```
