# `os.lazy.gpu.v1`

Interface id: `0x53ecd075e99e0064`

The Intel Gen9 GPU service (`gpud`, docs/kabylake-gpu-plan.md, stage G1:
"Adopt + observe").

**Read-only.** In this stage `gpud` claims the HD Graphics 620 of the
Kaby Lake box (PCI `8086:5916`), maps its register BAR and only *reads*
and decodes what the firmware left on the display: the pipe, transcoder
and DDI that are running, the link mode, the timing and pixel clock, the
core display clock, the first plane's surface and the GGTT pages behind
it. It writes no display register, programs no mode and takes no
interrupt, so nothing in this interface can change the hardware. It exists
to learn what the firmware leaves (plan section 2.1) and to give later
stages (HDMI audio, flips, decode) one place that already knows.

The state is a snapshot taken once when `gpud` starts; `GetBeam` is the
one live read (the frame counter and scan line); for any other live
register use `dbgd`'s `mmio.read`. Failures of a call are returned as a
structured error field (errno-style code, friendly text), not as a typed
reply. The interface is open to every caller: it reveals the display
mode and where its framebuffer is, nothing a program could not see by
asking the compositor. Method ordinals are append-only.

## Methods

| Method | Id | Kind | Signature |
|---|---|---|---|
| GetState | 746022616 | sync | `() -> (state: State)` |
| GetBeam | 400769696 | sync | `() -> (beam: Beam)` |

## struct `Axis`

- `active: U32`
- `total: U32`
- `blank_start: U32`
- `blank_end: U32`
- `sync_start: U32`
- `sync_end: U32`

## struct `Plane`

- `enabled: Bool`
- `format: U32`
- `tiling: U32`
- `stride_bytes: U32`
- `width: U32`
- `height: U32`
- `surface: U32`
- `surface_live: U32`

## struct `FbMap`

- `pages: U32`
- `present: U32`
- `phys_base: U64`
- `contiguous: Bool`

## struct `Beam`

- `frame: U32`
- `line: U32`
- `vtotal: U32`
- `at_ns: U64`
- `measured_millihz: U32`
- `underrun: U32`

## struct `State`

- `adopted: Bool`
- `device: U32`
- `revision: U32`
- `cdclk_khz: U32`
- `power_well_1: Bool`
- `power_well_2: Bool`
- `pipes_enabled: U32`
- `pipe: U32`
- `ddi_port: U32`
- `link: U32`
- `bits_per_component: U32`
- `hsync_positive: Bool`
- `vsync_positive: Bool`
- `pixel_clock_khz: U32`
- `refresh_millihz: U32`
- `horizontal: Axis`
- `vertical: Axis`
- `plane: Plane`
- `framebuffer: FbMap`
- `audio_capable: Bool`

## enum `LinkMode`

- None, Hdmi, Dvi, DpSst, DpMst, Other

## enum `Tiling`

- Linear, X, Y, Yf, Other

## enum `Underrun`

- None, Latched, Unobserved
