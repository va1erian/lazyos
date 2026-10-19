# Alder Lake box under LazyOS: what `dbgd` read (2026-10-19)

The first LazyOS boots of the LarkBox X (`../../README.md`), read over `dbgd`
(`tools/dbg/dbgctl.py --host <box>`). The first stick
(`--desktop --dbgd-control --usb-image`) gave the 64-byte PCI header and the
logs; the second (the same plus `--dbgd-mmio`, with the `8086:46b3` ranges of
`libs/pciprobe/src/builtin.rs`) gave the full config space and the display
registers: `02-mmio-gop.txt` (every allowed range but pipe D's, zeros left
out) and `03-ggtt-scanout.txt` (the first 3616 GGTT entries). Both sticks
booted from their RAM disk (`ram0p2`); NVMe and AHCI both attached. Nothing
had touched the display since the GOP: no LazyOS driver claims `00:02.0`.
This dump read the scaler coefficient and plane gamma data ports (`0x701D4`,
`0x701DC`, `0x70164` and their copies per plane and pipe) once (their
index registers are in auto-increment); the allowlist has since made them
holes, so those words in `02-mmio-gop.txt` are not the firmware's LUTs and a
later read cannot repeat it.

## What the firmware left (UEFI GOP)

```
LazyOS: framebuffer 2560x1440 Bgr
HW:FB:2560x1440->1920x1080 at 320,180 stride 2560 bpp 4     (early console)
HW:FB:WC:3600 pages write-combining
HW:FB:2560x1440->2560x1440 at 0,0 stride 2560 bpp 4
display: max 2560x1440 / mode 2560x1440 / console scale 2
```

The GOP mode is the monitor's native **2560x1440**, BGRX, stride 2560
pixels (10240 bytes), 3600 pages (14.06 MiB), at **59.95 Hz** (CVT reduced
blanking). This settles the README's "GOP mode *to confirm*".

## The firmware's display path (registers)

pipe A -> transcoder A -> DDI B (combo PHY B) -> HDMI, clocked by **DPLL1**.

| What | Register | Value | Meaning |
|---|---|---|---|
| Power wells | `PWR_WELL_CTL2` `0x45404` | `0x0000fc0f` | PW1, PW2 and the pipe A, B, C wells (idx 5-7) requested and on; **PW_D off**. `CTL1` (BIOS) `0x00005405` shows the states only |
| DC states | `DC_STATE_EN` `0x45504` | `0` | none; **DMC not loaded** (`DMC_HTP` `0x8F004` = 0, `DMC_SSP_BASE` = 0) |
| DBUF | `DBUF_CTL_S1`/`S2` | `0xc040c000` | both slices powered |
| CDCLK | `CDCLK_CTL` `0x46000`, `CDCLK_PLL_ENABLE` `0x46070` | `0x00380518`, `0xc0000022` | **652.8 MHz** (PLL ratio 34 x 38.4 MHz = 1305.6, /2), PLL enabled and locked |
| Timings | `HTOTAL_A`..`VSYNC_A` `0x60000`.. | `0a9f09ff 0a9f09ff 0a4f0a2f 05c8059f 05c8059f 05a705a2` | 2560/2608/2640/2720 x 1440/1443/1448/1481 (CVT-RB), no separate blanking |
| Pipe source | `PIPESRC_A` `0x6001C` | `0x09ff059f` | 2560x1440, no scaler (`PS_CTRL_1A` = 0) |
| Transcoder | `TRANS_DDI_FUNC_CTL_A` `0x60400` | `0x90010100` | enabled, port field 2 = **DDI B**, **HDMI** mode, 8 bpc, +HSync -VSync; bit 8 set (i915's `DP_VC_PAYLOAD_ALLOC`, meaning for HDMI unknown) |
| Infoframes | `VIDEO_DIP_CTL_A` `0x60200` | `0x00001000` | AVI only (Linux had GCP, AVI, SPD) |
| Transcoder clock | `TRANS_CLK_SEL_A` `0x46140` | `0x20000000` | port B |
| Pipe | `TRANSCONF_A` `0x70008`, `PIPEMISC_A` `0x70030` | `0xc0000000`, `0x01000000` | enabled and active, 8 bpc; pipes B and C off |
| DDI | `DDI_BUF_CTL_B` `0x64100` | `0x80000000` | enabled, not idle; A and TC1-4 idle (`0x80`) |
| Port clock | `DPCLKA_CFGCR0` `0x164280` | `0x01e07404` | DDI B clock on, from **DPLL1** (bits 3:2 = 01); DDI A gated |
| PLL | `DPLL1_ENABLE` `0x46014` | `0xcc000000` | enabled, locked, powered; DPLL0 and TBT off |
| PLL dividers | `DPLL1_CFGCR0`/`CFGCR1` `0x16428C` | `0x003c01b8`, `0x00000060` | DCO 19.2 MHz x (440 + 2 x 3840/32768) = 8452.5 MHz (the fraction is programmed halved on a 38.4 MHz reference, i915's Wa_22010492432), P=7 Q=1 K=1: **241.5 MHz** pixel clock. Linux's `i915_shared_dplls_info` showed these same values on its idle DPLL1 |
| Plane | `PLANE_CTL_1_A` `0x70180` | `0x84000008` | enabled, XRGB8888 (format bits 27:24 = 4), linear; bit 3 not decoded |
| | `PLANE_STRIDE_1_A`, `PLANE_SIZE_1_A` | `0xa0`, `0x059f09ff` | 160 x 64 = 10240 bytes, 2560x1440 |
| | `PLANE_SURF_1_A` `0x7019C` | `0` | GGTT offset 0 |
| | `PLANE_BUF_CFG_1_A`, `PLANE_WM_1_A_0` | `0x07ff0000`, `0x80004016` | DDB blocks 0-2047, level 0 WM 22 blocks |
| Hotplug | `SDEISR` `0xC4000` | `0x00020000` | DDI B live |
| Panel | `PP_STATUS`/`PP_CONTROL` | `0` | no eDP |

59.95 Hz from the clock: 241.5 MHz / (2720 x 1481). Measured from
`PIPE_FRMCOUNT_A` (`0x70040`) over dbgd: 1211 frames in 20.2 s = 59.95 Hz
(the read's network round trips bound the error). (A first decode without
the fraction workaround read 241.436 MHz, 59.93 Hz; the frame counter and
Linux's DPLL0, exactly 277.82 MHz with it, settle it.)

The scanout: GGTT entries 0..3599 are valid (`flags` 0x1) and map
**contiguous** pages `0x4c800000..0x4d610000`, the first 14.06 MiB of stolen
memory (`BDSM` = `0x4c800000`). So the GOP's framebuffer is stolen memory
seen through the aperture at `GMADR` + 0 (`0x40_0000_0000`).

### Against Linux (i915) on the same box

| | GOP (LazyOS) | i915 |
|---|---|---|
| Refresh, pixel clock | 59.95 Hz, 241.5 MHz (CVT-RB) | 70 Hz, 277.82 MHz |
| PLL | DPLL1 | DPLL0 |
| CDCLK | 652.8 MHz | (see `../03-dri*-i915_*`) |
| Plane format | XRGB8888, linear | `AB30` (10 bpc), Y-tiled through a DPT |
| Infoframes | AVI | GCP, AVI, SPD |
| DMC | not loaded | `adlp_dmc.bin` 2.20 |

A G1-style read-only adopter for display 13 therefore must read the PLL
from `DPCLKA_CFGCR0`, not assume DPLL0, and take `TRANS_DDI_FUNC_CTL`'s port
field with TGL's encoding (port + 1 in bits 30:27).

## PCI configuration header, `00:02.0` (`8086:46b3`)

```
0x000: 86 80 b3 46 07 00 10 00 0c 00 00 03 10 00 00 00
0x010: 04 00 00 00 60 00 00 00 0c 00 00 00 40 00 00 00
0x020: 01 50 00 00 00 00 00 00 00 00 00 00 86 80 a8 a7
0x030: 00 00 00 00 40 00 00 00 00 00 00 00 ff 01 00 00
```

| Field | Value |
|---|---|
| Revision, class | `0x0c`, `030000` (VGA) |
| Command | `0x0007`: I/O, memory, bus master on (as the firmware left it) |
| BAR0 (GTTMMADR) | 64-bit, non-prefetchable, `0x60_0000_0000` (16 MiB; same as Linux) |
| BAR2 (GMADR) | 64-bit, prefetchable, `0x40_0000_0000` (256 MiB aperture; same as Linux) |
| BAR4 (I/O) | `0x5000` |
| Subsystem | `8086:a7a8` |
| Capabilities | at `0x40`; pin INTA, line `0xff`; `dev:` routes it as "polling, msi" |

Past the header (second stick): `GMCH_CTRL` (`0x50`) = `0xfec1` (GMS
`0xfe`: 60 MiB stolen; GGMS 3: an 8 MiB GGTT), `BDSM` (`0xC0`, 64-bit on
display 12+) = `0x4c800001` (base `0x4c800000`, locked), `ASLS` (`0xFC`) =
`0x43978018` (the OpRegion, as under Linux). The host bridge agrees:
`MCHBAR` (`0x48`) `0xfedc0001`, `BGSM` (`0xB4`) `0x4c000001`, `BDSM`
(`0xB0`) `0x4c800001`, `TOLUD` (`0xBC`) `0x50400001`, `TSEG` (`0xB8`)
`0x4b000001`. Linux's `/proc/iomem` put stolen memory at
`0x4c800000-0x503fffff` (60 MiB).

The host bridge `00:00.0` is `8086:4601` rev `0x04`.

## Devices and drivers

`devd` matched nothing for `00:02.0` (`unmatched`): `gpud` is not started
(its manifest matches `8086:5916` only, and only in a `LAZYOS_GEN9GFX=1`, since renamed `LAZYOS_INTELGFX=1`
image). Both RTL8168h NICs are claimed by `netdrv`; one (`eth1`) later hit a
`tx timeout` and was detached. The HDA controller `8086:51c8` and the AX101
Wi-Fi `8086:51f0` are unmatched in this image.

CPU: 12 local APICs (`apic_ids=[0,1,8,9,16,18,20,22,24,26,28,30]`), x2APIC,
invariant TSC at 1996.8 MHz, LAPIC timer from CPUID 15 (38.4 MHz crystal);
I/O APIC with 120 pins. 31892 MiB usable, highest address `0x8afc00000`.

## Reading it again

```
python tools/run_demo.py --desktop --dbgd-control --dbgd-mmio --usb-image --build-only
python tools/dbg/dbgctl.py --host <box> pci-config 00:02.0 0 256
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x45400 16    # power wells first
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x60000 0x600  # transcoder A
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x70000 0x400  # pipe A, plane 1A
python tools/dbg/dbgctl.py --host <box> mmio 00:02.0 0 0x164280 0x40  # DPCLKA, DPLL CFGCR
```
